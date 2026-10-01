//! DocHost — per-chat `SessionDoc` handles: snapshot persistence (debounced), edge room
//! sync (offline-tolerant), and the HOST-ONLY durable command executor.
//!
//! Pragmatic port of luna's `session-docs.ts` + the `main.ts` executor (spec:
//! feature-inventory §3.3, ARCHITECTURE §2 "command plane"):
//! - the doc IS the outbox: commands and user entries commit locally and sync whenever a
//!   room connection exists; the engine is fully functional with sync disabled;
//! - on every doc change (local commit or remote import) the handle re-emits the joined
//!   transcript to watchers, drains pending commands, and schedules a snapshot save;
//! - command drain: evaluate via `evaluate_command` (with the DocsStore processed
//!   ledger), mark processed BEFORE execute, execute through the sessions engine, then
//!   write the outcome status back into the doc as the sole outcome writer.
//!
//! Chat ownership is gated on the workspace doc (`chats[chat_id].deviceId`), with
//! claim-on-first-command for unknown chats. Queueing a command for a chat hosted on
//! another device POSTs a durable nudge to that device's room (§7 cold-chat delivery);
//! the host's relay receives it and warm-opens the doc, which drains the queue.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use base64::Engine as _;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use tokio::sync::{OwnedMutexGuard, broadcast, watch};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use luna_doc::{
    COMMAND_DEFAULT_TTL_MS, CommandBasedOn, CommandDisposition, DocError, EvaluationContext,
    MessagePart, MessageRole, MessageStatus, QueueClaimMode, QueueClaimOutcome, QueueDeliveryGate,
    QueueHoldReason, QueueMessageStatus, QueuedMessage, RegistryDoc, SessionCommandEntry,
    SessionCommandPayload, SessionCommandStatus, SessionDoc, SessionMessageEntry, evaluate_command,
    join_continuation_entries,
};
use luna_proto::{
    CommandReasonCode, CommandReceipt, CommandReceiptReason, CommandReceiptStatus,
    ContextControlState, ConversationSourceContext, HarnessId, TaskCommandRead, UserInputAnswer,
    UserInputQuestion,
};
use luna_sync::{
    DocsStore, LocalExecutableChange, LocalExecutableKind, TaskCommandBegin, TaskRecoveryTransition,
};

use crate::http_error::describe_http_error;
use crate::sessions::{SessionsEngine, SteerOutcome};
use crate::workspace_host::{TaskProjectionError, WorkspaceHost};
use crate::{EngineError, new_id, now_ms};

/// Debounce window for local snapshot saves after a doc change.
const SNAPSHOT_DEBOUNCE_MS: u64 = 1_000;

/// An edit client renews every 20s. Sixty seconds tolerates two missed
/// heartbeats without turning a vanished client into an invisible permanent
/// lock. Expiry fails closed into ReviewRequired rather than releasing.
pub const QUEUE_EDIT_LEASE_MS: i64 = 60_000;

/// Warm-doc LRU: how many unwatched, run-less docs stay fully open. Everything
/// beyond this (and beyond [`luna_doc::DOC_LRU_BYTE_BUDGET`]) is evicted
/// oldest-access-first — reopening from the SQLite snapshot measured within
/// ~11ms of a warm doc, so the cap trades no perceptible open latency.
const WARM_DOC_CAP: usize = 12;

/// Resident-memory estimate per compressed snapshot byte. Loro snapshots are
/// columnar+compressed; the in-memory doc plus mirror runs well above the blob
/// size. A rough multiplier is enough here — the budget is a safety ceiling,
/// the count cap does the day-to-day work.
const RESIDENT_BYTES_PER_SNAPSHOT_BYTE: usize = 6;

/// Floor per open doc (room socket buffers, tasks) regardless of content size.
const DOC_RESIDENT_FLOOR_BYTES: usize = 512 * 1024;

/// Docs touched this recently are never evicted. Closes the open→attach race:
/// `open()` returns a handle, and until the caller's `watch_messages` lands
/// the doc is unwatched and unpinned — a concurrent eviction would orphan the
/// watcher on a roomless doc that renders once and never updates again.
const EVICT_MIN_IDLE_MS: i64 = 30_000;

/// Queued-attachment transfer pacing: chunk pushes are bounded per call (a
/// stalled-but-open relay link never fails on its own) and a timeout marks
/// the link suspect; attempts retry on this backoff, cut short by the online
/// bus / system wake.
const TRANSFER_CHUNK_B64: usize = 60_000;
const TRANSFER_CHUNK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const TRANSFER_COMMIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const TRANSFER_BACKOFF_BASE: std::time::Duration = std::time::Duration::from_secs(2);
const TRANSFER_BACKOFF_CAP: std::time::Duration = std::time::Duration::from_secs(30);
/// A command whose attachment bytes are still in transit waits at most this
/// long before the drain rejects it loudly (and the transfer task gives up on
/// the same clock) — a chat must never wedge behind bytes that aren't coming.
const ATTACHMENT_WAIT_MAX: std::time::Duration = std::time::Duration::from_secs(15 * 60);
const ATTACHMENT_WAIT_MAX_MS: i64 = ATTACHMENT_WAIT_MAX.as_millis() as i64;
/// Re-check cadence while a chat's queue is deferred on in-transit bytes
/// (the happy path is event-driven — UploadCommit kicks the drain — this
/// timer only covers the give-up transition and missed kicks).
const ATTACHMENT_WAIT_RECHECK: std::time::Duration = std::time::Duration::from_secs(30);

/// Transfer attempt outcome: transient failures retry (the link may heal),
/// permanent ones stop (the host actively refused, or the bytes are gone).
enum TransferError {
    Transient(String),
    Permanent(String),
}

/// Peer-relay delivery fallback pacing (`spawn_command_delivery`): the grace
/// the normal rows→edge path gets before the relay road opens, the poll while
/// waiting, the relay retry curve, its per-call deadline, and the give-up cap
/// (the command stays durably queued in the doc regardless).
const ROWS_GRACE: std::time::Duration = std::time::Duration::from_secs(10);
const ROWS_POLL: std::time::Duration = std::time::Duration::from_secs(1);
const RELAY_BACKOFF_BASE: std::time::Duration = std::time::Duration::from_secs(5);
const RELAY_BACKOFF_CAP: std::time::Duration = std::time::Duration::from_secs(30);
const RELAY_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const RELAY_GIVE_UP: std::time::Duration = std::time::Duration::from_secs(15 * 60);
/// Hosts below this stamped engine version don't serve `RelayCommand`.
const RELAY_MIN_VERSION: (u64, u64, u64) = (0, 2, 12);

/// Edge connection config. The bearer is a **provider**, never a snapshot:
/// every room (re)connect and HTTP request re-reads it, so WorkOS access-token
/// refreshes (~1h expiry) take effect without an engine restart. Dev bearers
/// (which never expire) ride the same seam as a [`luna_rpc::StaticToken`].
#[derive(Clone)]
pub struct EdgeConfig {
    /// Edge base URL (`http(s)://…`); rewritten to `ws(s)` for the room socket.
    pub url: String,
    /// Fresh-bearer provider (the relay's `TokenSource`), consulted per
    /// connect/request. Temporary failures preserve the signed-in session.
    pub token: Arc<dyn luna_rpc::TokenSource>,
    /// This engine's device id, carried on room dials (`&device=`) so the
    /// edge can attribute sockets in logs. Debugging the 2026-08-04 deaf
    /// socket meant reverse-engineering devices from rotating IPv6 privacy
    /// addresses; never again. Empty = omitted (tests).
    pub device_id: String,
}

impl std::fmt::Debug for EdgeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeConfig")
            .field("url", &self.url)
            .field("token", &"<provider>")
            .finish()
    }
}

impl EdgeConfig {
    pub fn new(url: impl Into<String>, token: Arc<dyn luna_rpc::TokenSource>) -> Self {
        Self {
            url: url.into(),
            token,
            device_id: String::new(),
        }
    }

    /// Attribute this engine's room sockets in edge logs.
    pub fn with_device(mut self, device_id: impl Into<String>) -> Self {
        self.device_id = device_id.into();
        self
    }

    /// Fixed bearer — dev mode and tests, where tokens never expire.
    pub fn with_static_token(url: impl Into<String>, token: impl Into<String>) -> Self {
        Self::new(url, Arc::new(luna_rpc::StaticToken(token.into())))
    }

    /// The current bearer, or a distinct signed-out/temporarily-unavailable error.
    pub async fn bearer(&self) -> Result<String, luna_rpc::TokenError> {
        self.token.token().await
    }

    pub fn token_changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.token.subscribe()
    }

    /// A per-dial room URL provider for `path` (e.g. `/session/{chatId}/ws`):
    /// the bearer is re-fetched before every connect, so reconnects after a
    /// token expiry present a fresh `?token=` instead of the boot-time one.
    pub fn room_url(&self, path: impl Into<String>) -> Arc<dyn luna_sync::UrlProvider> {
        let ws_base = self.url.replacen("http", "ws", 1);
        Arc::new(EdgeRoomUrl {
            base: format!("{}{}", ws_base.trim_end_matches('/'), path.into()),
            token: self.token.clone(),
            device_id: self.device_id.clone(),
        })
    }
}

struct EdgeRoomUrl {
    base: String,
    token: Arc<dyn luna_rpc::TokenSource>,
    device_id: String,
}

impl luna_sync::UrlProvider for EdgeRoomUrl {
    fn url(&self) -> futures::future::BoxFuture<'static, Result<String, luna_sync::SyncError>> {
        let token = self.token.clone();
        let base = self.base.clone();
        let device = self.device_id.clone();
        Box::pin(async move {
            let token = token.token().await.map_err(luna_sync::SyncError::from)?;
            let mut url = format!("{base}?token={token}");
            if !device.is_empty() {
                url.push_str(&format!("&device={device}"));
            }
            Ok(url)
        })
    }
}

#[derive(Debug, Clone)]
pub struct DocHostConfig {
    pub device_id: String,
    /// Harness for doc-command runs on chats without a workspace `config` row.
    pub default_harness: HarnessId,
    /// When present, each opened chat joins its edge session room. `None` = fully
    /// offline operation (local snapshots only).
    pub edge: Option<EdgeConfig>,
}

struct DocHostInner {
    store: Arc<DocsStore>,
    config: DocHostConfig,
    /// Set-once (first wins), cleared by `shutdown_workers`: sessions and
    /// doc-host reference each other through Arcs, so a retired runtime's
    /// graph only drops once this back-edge is severed.
    sessions: Mutex<Option<SessionsEngine>>,
    workspace: OnceLock<WorkspaceHost>,
    /// Worktree materialization for Run commands (see `set_repos`).
    repos: OnceLock<crate::repos::Repos>,
    project_commands: OnceLock<crate::project_commands::ProjectCommands>,
    /// Cancels every worker spawned through `spawn_worker` — the loops'
    /// own exit conditions (weak handle death, closed channels) don't cover
    /// runtime replacement, where Edge-capable tasks must stop doing
    /// network work even while something still pins the graph.
    shutdown: CancellationToken,
    edge_disconnected: AtomicBool,
    /// Tracks every spawned worker so `shutdown_workers` can await them.
    tasks: TaskTracker,
    handles: Mutex<HashMap<String, Arc<ChatDocHandle>>>,
    /// Serialize cold opens without blocking access to already-live handles.
    opening: Mutex<()>,
    /// First open in this engine process reconciles a durable `Running`
    /// compaction left by an interrupted predecessor to `Unknown`. Subsequent
    /// LRU reopens must not disturb an operation started by this process.
    context_reconciled: Mutex<HashSet<String>>,
    /// chat2 seeds in flight (one per chat — reopen storms must not race
    /// duplicate rebuild+checkpoint POSTs; benign server-side, wasteful).
    seeding: Mutex<HashSet<String>>,
    /// chat2 quiet-waiters armed (one per chat): the cutover watcher re-arms
    /// on every registry change, and a long run would stack a tick loop per
    /// change without this.
    seed_waiting: Mutex<HashSet<String>>,
    /// Attachment-wait re-drain timers armed (one per chat): a command
    /// deferred on in-transit attachment bytes re-checks on a cadence, and
    /// each deferral must not stack another timer.
    drain_waiting: Mutex<HashSet<String>>,
    /// Uploads store (engine assembly) — resolves `pending://` attachment
    /// refs and jails transfer reads to the uploads dir.
    uploads: OnceLock<crate::uploads::Uploads>,
    /// Connectivity watch (`WatchConnectivity`): lazily-started monitor
    /// publishes the edge posture on change (see `watch_connectivity`).
    connectivity: OnceLock<watch::Sender<luna_proto::Connectivity>>,
    connectivity_started: AtomicBool,
    /// In-flight queued-attachment transfers, published per landed chunk
    /// (see `watch_transfers`). Entries live exactly as long as bytes are
    /// moving: added when a file's push starts, removed on commit or failure
    /// (a retry re-adds), so consumers can render a real percent while the
    /// relay leg runs and fall back to indeterminate otherwise.
    transfers: watch::Sender<Vec<luna_proto::TransferProgress>>,
    connectivity_grace: Mutex<DegradeGrace>,
    /// Command ids currently BETWEEN mark-processed and their resolution in a
    /// drain. Distinguishes "executing right now" from "consumed by the
    /// ledger but dead" (a crash between mark and resolve): the drain
    /// terminalizes the latter as Rejected instead of leaving a forever-
    /// Pending entry no retry could ever reach (2026-08-19 swallowed-send).
    executing: Mutex<HashMap<String, String>>,
    /// L07 execution lanes. SQLite receipts are the correctness boundary; this
    /// gate only prevents avoidable local interleaving between acceptance and
    /// effect finalization.
    task_command_gates: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Stable across cold opens/LRU replacement, so repair admission never
    /// needs to open a document (and run its recovery hooks) just to fence it.
    queue_drain_gates: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Receipts publish only after their SQLite transaction commits.
    task_receipts: broadcast::Sender<CommandReceipt>,
    #[cfg(test)]
    approved_queue_postcommit_fail_once: AtomicBool,
    /// Peer links (engine assembly, edge runtimes only) — the transport that
    /// pushes queued attachment bytes to a remote host.
    links: OnceLock<Arc<luna_rpc::LinkCache>>,
    /// Shared client for sidecar blob PUT/GET (30s timeout, uploads.rs
    /// discipline — diff_sync's untimed client hung on dead links).
    http: reqwest::Client,
}

/// Held only around a local workspace association transaction. No provider
/// work or document-open recovery is performed while acquiring it.
pub struct WorkspaceRepairGuard {
    _drain: OwnedMutexGuard<()>,
    _execution: OwnedMutexGuard<()>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The queued-attachment transfers a command's `pending://` refs imply —
/// shared by the retry escort and the retry re-issue.
fn command_transfers(entry: &SessionCommandEntry) -> Vec<crate::uploads::AttachmentTransfer> {
    let refs: Vec<String> = match &entry.payload {
        SessionCommandPayload::Run { request, .. } => request
            .attachments
            .iter()
            .filter(|p| crate::uploads::is_pending_ref(p))
            .cloned()
            .collect(),
        SessionCommandPayload::Steer {
            prompt,
            attachments,
            ..
        } => {
            let mut refs = crate::uploads::pending_refs_in(prompt);
            refs.extend(
                attachments
                    .iter()
                    .filter(|p| crate::uploads::is_pending_ref(p))
                    .cloned(),
            );
            refs.sort();
            refs.dedup();
            refs
        }
        _ => Vec::new(),
    };
    refs.iter()
        .filter_map(|r| crate::uploads::parse_pending_ref(r))
        .map(
            |(upload_id, file_name)| crate::uploads::AttachmentTransfer {
                upload_id: upload_id.to_string(),
                file_name: file_name.to_string(),
            },
        )
        .collect()
}

/// Retires a transfer's progress entry on drop — the one exit point for
/// `push_attachments`' many returns (commit landed, chunk timeout, host
/// refusal), so no failure path can leave a phantom ring behind.
struct TransferProgressGuard<'a> {
    host: &'a DocHost,
    upload_id: &'a str,
}

impl Drop for TransferProgressGuard<'_> {
    fn drop(&mut self) {
        self.host.transfer_progress_clear(self.upload_id);
    }
}

/// How long raw degradation must persist before connectivity reports it.
/// Room joins, idle-link wakes, and navigation dials resolve well under a
/// second on healthy networks; real outages outlive this comfortably. Recovery
/// is never delayed.
const DEGRADE_GRACE: std::time::Duration = std::time::Duration::from_secs(4);

/// One tracked degradation source in [`DegradeGrace`].
enum GraceKey<'a> {
    OsPath,
    Registry,
    Chat(&'a str),
}

/// Show-slow / hide-fast hysteresis over raw connectivity signals. Pure over
/// injected `Instant`s so the grace window is unit-testable.
#[derive(Default)]
struct DegradeGrace {
    os_path: Option<std::time::Instant>,
    registry: Option<std::time::Instant>,
    chats: HashMap<String, std::time::Instant>,
}

impl DegradeGrace {
    /// Feed one raw sample; returns whether to REPORT the source as degraded.
    /// Healthy clears the timer instantly; degraded reports only once it has
    /// persisted for [`DEGRADE_GRACE`].
    fn degraded(&mut self, key: GraceKey, raw: bool, now: std::time::Instant) -> bool {
        let slot: &mut Option<std::time::Instant> = match key {
            GraceKey::OsPath => &mut self.os_path,
            GraceKey::Registry => &mut self.registry,
            GraceKey::Chat(id) => {
                if raw && !self.chats.contains_key(id) {
                    self.chats.insert(id.to_string(), now);
                }
                match self.chats.get_mut(id) {
                    Some(_) if !raw => {
                        self.chats.remove(id);
                        return false;
                    }
                    Some(since) => return now.duration_since(*since) >= DEGRADE_GRACE,
                    None => return false,
                }
            }
        };
        if !raw {
            *slot = None;
            return false;
        }
        let since = *slot.get_or_insert(now);
        now.duration_since(since) >= DEGRADE_GRACE
    }

    /// Drop timers for chats that no longer have open docs.
    fn retain_chats(&mut self, keep: impl Fn(&str) -> bool) {
        self.chats.retain(|id, _| keep(id));
    }
}

#[derive(Clone)]
pub struct DocHost {
    inner: Arc<DocHostInner>,
}

/// How a taken queue row reaches the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueSend {
    /// Nothing is running: start a turn with it.
    NextTurn,
    /// Something is running and can take input mid-turn: steer it in.
    Steer,
    /// The user said now: stop what is running first.
    Interrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueProviderOutcome {
    Accepted,
    Rejected(QueueHoldReason),
    Unknown,
}

const ATTACHMENT_ONLY_PROMPT: &str = "See the attached image(s).";
const ATTACHMENT_PROMPT_HEADER: &str = "Attached images (local files — open them to view):";

/// Queue rows keep the user's editable text separate from attachment paths.
/// Rebuild the transcript/harness transport only when the row is dispatched.
///
/// Older clients stored the already-expanded prompt in `text`; recognize the
/// exact trailer implied by `attachments` so those rows are not expanded a
/// second time after an upgrade.
fn queued_message_prompt(text: &str, attachments: &[String]) -> String {
    if attachments.is_empty() {
        return text.to_string();
    }
    let refs = attachments
        .iter()
        .map(|path| format!("- {path}"))
        .collect::<Vec<_>>()
        .join("\n");
    let trailer = format!("\n\n{ATTACHMENT_PROMPT_HEADER}\n{refs}");
    let body = text.strip_suffix(&trailer).unwrap_or(text);
    let body = if body.trim().is_empty() {
        ATTACHMENT_ONLY_PROMPT
    } else {
        body
    };
    format!("{body}{trailer}")
}

fn queue_text_hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "outcome",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BeginQueueEditOutcome {
    Acquired {
        lease_id: String,
        text: String,
        attachments: Vec<String>,
        input_items: Vec<luna_proto::ComposerInput>,
        base_text_hash: String,
        revision: u64,
        expires_at_ms: i64,
    },
    Locked {
        owner_device_id: String,
        expires_at_ms: i64,
    },
    Missing,
    Claimed {
        command_id: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "outcome",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RenewQueueEditOutcome {
    Renewed { expires_at_ms: i64 },
    Lost,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishQueueEditAction {
    Commit,
    Cancel,
    Discard,
    ReleaseUnchanged,
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "outcome",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FinishQueueEditOutcome {
    Committed,
    Cancelled,
    Discarded,
    Released,
    Conflict { current_text: String },
    RevisionConflict { current_revision: u64 },
    Claimed { command_id: String },
    Lost,
    Missing,
}

#[derive(Debug, Clone, PartialEq)]
pub enum QueueMutationOutcome {
    Applied,
    Duplicate,
    RevisionConflict(QueuedMessage),
    Claimed(QueuedMessage),
    Missing,
}

#[derive(Debug, Clone, PartialEq)]
pub enum QueueDispatchOutcome {
    Started,
    Rejected(QueuedMessage),
    Unknown(QueuedMessage),
    AlreadyClaimed(QueuedMessage),
    RevisionConflict(QueuedMessage),
    Blocked(QueuedMessage),
    Missing,
}

struct PreparedQueueProjection<T> {
    outcome: T,
    snapshot: Vec<u8>,
    update: Vec<u8>,
    batch_id: Option<String>,
    authority_changes: Vec<LocalExecutableChange>,
    unreceipted_new_commands: Vec<(String, String)>,
}

pub(crate) fn executable_row_hash<T: Serialize>(row: &T) -> Result<String, EngineError> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(row)?)))
}

#[allow(clippy::too_many_arguments)]
fn finish_queue_edit_projection(
    doc: &SessionDoc,
    id: &str,
    lease_id: &str,
    action: FinishQueueEditAction,
    text: Option<&str>,
    expected_text_hash: Option<&str>,
    attachments: Option<&[String]>,
    input_items: Option<&[luna_proto::ComposerInput]>,
    expected_revision: Option<u64>,
) -> Result<(FinishQueueEditOutcome, bool), EngineError> {
    let Some(item) = doc.read_queue()?.into_iter().find(|item| item.id == id) else {
        return Ok((FinishQueueEditOutcome::Missing, false));
    };
    if let QueueMessageStatus::Claimed { command_id, .. }
    | QueueMessageStatus::SteeringAccepted { command_id, .. }
    | QueueMessageStatus::DeliveryUnknown { command_id, .. } = &item.status
    {
        return Ok((
            FinishQueueEditOutcome::Claimed {
                command_id: command_id.clone(),
            },
            false,
        ));
    }
    if expected_revision.is_some_and(|revision| revision != item.revision) {
        return Ok((
            FinishQueueEditOutcome::RevisionConflict {
                current_revision: item.revision,
            },
            false,
        ));
    }
    let (current_lease, base_text_hash) = match &item.delivery_gate {
        Some(QueueDeliveryGate::Editing {
            lease_id,
            base_text_hash,
            ..
        }) => (lease_id, base_text_hash),
        Some(QueueDeliveryGate::ReviewRequired {
            previous_lease_id,
            base_text_hash,
            ..
        }) => (previous_lease_id, base_text_hash),
        None => return Ok((FinishQueueEditOutcome::Lost, false)),
    };
    if current_lease != lease_id {
        return Ok((FinishQueueEditOutcome::Lost, false));
    }
    if action == FinishQueueEditAction::Commit
        && (expected_text_hash != Some(base_text_hash.as_str())
            || queue_text_hash(&item.text) != *base_text_hash)
    {
        return Ok((
            FinishQueueEditOutcome::Conflict {
                current_text: item.text,
            },
            false,
        ));
    }
    let replacement = match action {
        FinishQueueEditAction::Commit => Some(text.unwrap_or_default()),
        FinishQueueEditAction::Cancel | FinishQueueEditAction::ReleaseUnchanged => None,
        FinishQueueEditAction::Discard => Some(""),
    };
    if !doc.finish_queued_edit_with_inputs(
        id,
        replacement,
        if action == FinishQueueEditAction::Commit {
            attachments
        } else {
            None
        },
        if action == FinishQueueEditAction::Commit {
            input_items
        } else {
            None
        },
        now_ms(),
    )? {
        return Ok((FinishQueueEditOutcome::Missing, false));
    }
    Ok((
        match action {
            FinishQueueEditAction::Commit => FinishQueueEditOutcome::Committed,
            FinishQueueEditAction::Cancel => FinishQueueEditOutcome::Cancelled,
            FinishQueueEditAction::Discard => FinishQueueEditOutcome::Discarded,
            FinishQueueEditAction::ReleaseUnchanged => FinishQueueEditOutcome::Released,
        },
        true,
    ))
}

/// Content and its historical presentation cutoff travel atomically, even
/// when the watch coalesces several backfill and live commits.
#[derive(Clone, Default)]
pub struct TranscriptSnapshot {
    pub entries: Arc<Vec<SessionMessageEntry>>,
    pub replay_baseline: Arc<luna_doc::TranscriptBaseline>,
}

/// One open chat doc: the `SessionDoc`, its change plumbing, and the room client.
pub struct ChatDocHandle {
    chat_id: String,
    device_id: String,
    doc: Arc<SessionDoc>,
    messages_tx: watch::Sender<TranscriptSnapshot>,
    /// Serialize historical imports with publication so an async doc-change
    /// task cannot publish recovered content before its presentation cutoff.
    transcript_import: Mutex<()>,
    transcript_history: Arc<Mutex<crate::transcript_history::TranscriptHistory>>,
    /// Pending-message queue watch (WatchQueue). Cheap to rebuild — a handful
    /// of short rows — so unlike the transcript mirror it publishes on every
    /// change without a dirty flag.
    queue_tx: watch::Sender<Vec<QueuedMessage>>,
    /// Serializes everything that TAKES from the queue. Both the doc-change
    /// task and the turn-end status watcher call `drain_queue`, and nothing
    /// keeps those two apart: without this they interleave across the
    /// `dispatch` await, each taking a different head and each sending, so a
    /// queue meant to release one message releases all of them.
    ///
    /// It also covers the gap a send-now's interrupt opens — between stopping
    /// the turn and starting its own the chat reads Idle, and an idle chat with
    /// a queue is exactly what the flush drains.
    drain_lock: Arc<tokio::sync::Mutex<()>>,
    /// Serializes candidate queue projections through durable commit and
    /// post-commit installation. Queue mutations are built off-doc, so a
    /// failed SQLite transaction is never observable through this handle.
    queue_projection_lock: Mutex<()>,
    /// A committed projection has not reached the live doc. Never let a
    /// debounced snapshot of that older doc replace the committed bytes.
    durable_projection_uninstalled: AtomicBool,
    /// An explicit user interrupt freezes automatic queue delivery. The next
    /// explicit prompt or queue send resumes it; incidental doc/status changes
    /// must not turn Cancel into "send the next row".
    queue_paused: AtomicBool,
    /// True when the doc changed while nobody watched: the mirror rebuild is
    /// deferred to the next `watch_messages` attach instead of paid per commit.
    mirror_dirty: AtomicBool,
    /// Epoch ms of the last open/watch touch — the LRU eviction key.
    last_access: AtomicI64,
    /// Last known snapshot blob size — the eviction budget estimate's input.
    snapshot_bytes: AtomicUsize,
    /// The sync generation this handle was BUILT for (1 = legacy s2,
    /// 2 = chat2). Gen-1 handles no longer join any room (the s2 client is
    /// gone); they serve the local fat doc read-only until the host's seed
    /// flips the chat to chat2. The staleness checks compare this against
    /// the registry — inferring mode from `chat2_local_sub` misread
    /// edge-less chat2 handles (no subscription is ever installed offline)
    /// as stale s2 and retired them on every open, dropping the doc out
    /// from under live runs.
    room_gen: u32,
    /// A threshold checkpoint POST is in flight (review H1: the quiesce
    /// tick must not stack concurrent full-snapshot uploads).
    checkpointing: Arc<AtomicBool>,
    /// Set when a chat2 seed replaced this handle's lineage on disk: every
    /// further snapshot save from this handle is a stale FAT doc that would
    /// clobber the thin one — retired handles never persist again (unless no
    /// thin lineage exists on disk at all; `save_snapshot` double-checks, so
    /// a doc that was never seeded can't lose its only copy).
    retired: AtomicBool,
    /// chat2 relay client (docs/chat2-sync.md C3) — populated once the
    /// registry names roomGen 2 for this chat and the join resolves.
    chat2: Mutex<Option<luna_sync::ChatClient>>,
    /// Local commits made before the relay connects (the dial can take up
    /// to a minute; offline, forever): buffered here by the subscription
    /// below and drained into the client on join (review B3 — a user
    /// message typed during the dial must not silently never sync).
    chat2_pending_local: Mutex<Vec<(String, Vec<u8>)>>,
    publication_failed: AtomicBool,
    /// Local-update feed into the chat2 client (drop = unsubscribe).
    chat2_local_sub: Mutex<Option<loro::Subscription>>,
    /// Doc subscription (drop = unsubscribe) — bumps the change watch on every commit.
    _sub: loro::Subscription,
}

impl ChatDocHandle {
    pub fn chat_id(&self) -> &str {
        &self.chat_id
    }

    pub fn doc(&self) -> &SessionDoc {
        &self.doc
    }

    pub fn doc_arc(&self) -> Arc<SessionDoc> {
        self.doc.clone()
    }

    /// Joined transcript watch — re-sent on every doc change (WatchDocMessages).
    ///
    /// Attach-time refresh: the mirror is only maintained while watched, so a
    /// doc that changed unwatched materializes here, once, instead of on every
    /// commit it sat through in the background.
    pub fn watch_messages(&self) -> watch::Receiver<TranscriptSnapshot> {
        self.touch();
        // Attach is a user signal: verify a quiet room is actually alive
        // (a doc-wedged DO keeps answering pings while delivering nothing,
        // and the background probe cadence can be hours out). Coalescing
        // no-op on a healthy or recently-active room.
        if let Some(chat2) = lock(&self.chat2).as_ref() {
            chat2.probe();
        }
        // Subscribe BEFORE the dirty check: a commit racing this attach then
        // sees a live receiver and publishes, instead of re-marking dirty
        // after our refresh and leaving the new watcher a cleared mirror.
        let rx = {
            let _import = lock(&self.transcript_import);
            if self.messages_tx.receiver_count() == 0 {
                // A new viewing session must not inherit the former viewer's
                // live-part protection, even if no commit happened while away.
                *lock(&self.transcript_history) = Default::default();
            }
            self.messages_tx.subscribe()
        };
        if self.mirror_dirty.load(Ordering::Acquire) {
            self.publish_messages();
        }
        rx
    }

    /// Queue watch — the composer's held messages, re-sent on every doc change.
    pub fn watch_queue(&self) -> watch::Receiver<Vec<QueuedMessage>> {
        self.touch();
        let rx = self.queue_tx.subscribe();
        self.publish_queue();
        rx
    }

    fn publish_queue(&self) {
        match self.doc.read_queue() {
            Ok(items) => {
                self.queue_tx.send_if_modified(|slot| {
                    if *slot == items {
                        false
                    } else {
                        *slot = items;
                        true
                    }
                });
            }
            Err(err) => {
                tracing::warn!(chat = %self.chat_id, error = %err, "queue read failed");
            }
        }
    }

    fn touch(&self) {
        self.last_access.store(now_ms(), Ordering::Relaxed);
    }

    pub fn connected(&self) -> bool {
        lock(&self.chat2).is_some()
    }

    /// Write a complete user message entry, idempotent by id (the client-minted message
    /// id — a re-executed command or optimistic echo never duplicates the entry).
    pub fn write_user_message(
        &self,
        message_id: &str,
        text: &str,
        created_at: i64,
    ) -> Result<(), DocError> {
        if self.doc.read_entries()?.iter().any(|e| e.id == message_id) {
            return Ok(());
        }
        self.doc.push_message(&SessionMessageEntry {
            id: message_id.to_string(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "t0".into(),
                text: text.to_string(),
            }],
            created_at,
            device_id: self.device_id.clone(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        })
    }

    /// Recovery sweep: stamp this device's abandoned `streaming` entries `aborted`, appending
    /// `note` as a visible error part so the transcript says WHY the turn
    /// ended (luna folded "Run interrupted by backend restart" the same
    /// way). Returns the stamped entries' `(id, created_at)` — recovery uses
    /// them for the resume-freshness check.
    pub fn mark_abandoned_streams(&self, note: &str) -> Result<Vec<(String, i64)>, DocError> {
        let mut stamped = Vec::new();
        for entry in self.doc.read_entries()? {
            if entry.role == MessageRole::Assistant
                && entry.status == Some(MessageStatus::Streaming)
                && entry.device_id == self.device_id
                && self
                    .doc
                    .set_message_status(&entry.id, MessageStatus::Aborted)?
            {
                let part_id = format!("{}-recovery", entry.id);
                if let Err(err) = self.doc.append_error_part(&entry.id, &part_id, note) {
                    tracing::warn!(chat = %self.chat_id, error = %err, "recovery note append failed");
                }
                stamped.push((entry.id.clone(), entry.created_at));
            }
        }
        if !stamped.is_empty() {
            self.publish_messages();
        }
        Ok(stamped)
    }

    fn publish_messages(&self) {
        let _import = lock(&self.transcript_import);
        self.mirror_dirty.store(false, Ordering::Release);
        match self.doc.read_entries() {
            Ok(entries) => {
                let replay_baseline =
                    lock(&self.transcript_history).snapshot(self.doc.doc(), &entries);
                let joined = join_continuation_entries(entries);
                // send_replace: update the watch even with no subscribers yet, so a
                // late subscriber's first borrow sees the current transcript.
                self.messages_tx.send_replace(TranscriptSnapshot {
                    entries: Arc::new(joined),
                    replay_baseline: replay_baseline.clone(),
                });
            }
            Err(err) => {
                tracing::warn!(chat = %self.chat_id, error = %err, "transcript read failed");
            }
        }
    }

    /// A handle-backed chat2 import also writes the room cursor. Serialize
    /// both the live import and that SQLite write with detached projections,
    /// then refuse a stale handle after a committed projection failed to
    /// install. The caller must leave the remote cursor unadvanced on None.
    pub(crate) fn import_transcript<T>(&self, import: impl FnOnce() -> T) -> Option<T> {
        let _projection = lock(&self.queue_projection_lock);
        if self.durable_projection_uninstalled.load(Ordering::Acquire) {
            return None;
        }
        let _guard = lock(&self.transcript_import);
        Some(import())
    }

    /// Per-commit publish path: unwatched docs just mark the mirror dirty —
    /// rebuilding a full transcript nobody reads was a per-tick cost on every
    /// open doc (and kept a second transcript copy hot).
    fn publish_messages_if_watched(&self) {
        if self.messages_tx.receiver_count() == 0 {
            self.mirror_dirty.store(true, Ordering::Release);
            // Shrink the stale mirror: watch_messages rebuilds on attach.
            self.messages_tx.send_replace(TranscriptSnapshot::default());
            *lock(&self.transcript_history) = Default::default();
        } else {
            self.publish_messages();
        }
    }

    /// Rough resident cost for the LRU budget.
    fn resident_estimate(&self) -> usize {
        (self.snapshot_bytes.load(Ordering::Relaxed) * RESIDENT_BYTES_PER_SNAPSHOT_BYTE)
            .max(DOC_RESIDENT_FLOOR_BYTES)
    }
}

impl DocHost {
    pub fn new(store: Arc<DocsStore>, config: DocHostConfig) -> Self {
        Self {
            inner: Arc::new(DocHostInner {
                store,
                config,
                sessions: Mutex::new(None),
                workspace: OnceLock::new(),
                repos: OnceLock::new(),
                project_commands: OnceLock::new(),
                shutdown: CancellationToken::new(),
                edge_disconnected: AtomicBool::new(false),
                tasks: TaskTracker::new(),
                handles: Mutex::new(HashMap::new()),
                opening: Mutex::new(()),
                context_reconciled: Mutex::new(HashSet::new()),
                seeding: Mutex::new(HashSet::new()),
                seed_waiting: Mutex::new(HashSet::new()),
                drain_waiting: Mutex::new(HashSet::new()),
                uploads: OnceLock::new(),
                connectivity: OnceLock::new(),
                connectivity_started: AtomicBool::new(false),
                transfers: watch::channel(Vec::new()).0,
                connectivity_grace: Mutex::new(DegradeGrace::default()),
                executing: Mutex::new(HashMap::new()),
                task_command_gates: Mutex::new(HashMap::new()),
                queue_drain_gates: Mutex::new(HashMap::new()),
                task_receipts: broadcast::channel(256).0,
                #[cfg(test)]
                approved_queue_postcommit_fail_once: AtomicBool::new(false),
                links: OnceLock::new(),
                http: reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()
                    .unwrap_or_else(|_| reqwest::Client::new()),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_next_approved_queue_install_for_test(&self) {
        self.inner
            .approved_queue_postcommit_fail_once
            .store(true, Ordering::Release);
    }

    /// Every background task rides the tracker, raced against the shutdown
    /// token: the loops' own exits stay authoritative in normal operation;
    /// the token is the retirement override.
    fn spawn_worker(&self, fut: impl std::future::Future<Output = ()> + Send + 'static) {
        let cancel = self.inner.shutdown.clone();
        self.inner.tasks.spawn(async move {
            tokio::select! {
                _ = cancel.cancelled() => {}
                _ = fut => {}
            }
        });
    }

    /// `spawn_worker` for sites that pre-resolve a runtime handle (callers
    /// reachable from bare sync contexts, where `tasks.spawn` would panic).
    fn spawn_worker_on(
        &self,
        runtime: &tokio::runtime::Handle,
        fut: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        let cancel = self.inner.shutdown.clone();
        self.inner.tasks.spawn_on(
            async move {
                tokio::select! {
                    _ = cancel.cancelled() => {}
                    _ = fut => {}
                }
            },
            runtime,
        );
    }

    /// The sessions engine, once wired. `None` before assembly or after
    /// `shutdown_workers` — callers treat both as "executor unavailable".
    fn sessions(&self) -> Option<SessionsEngine> {
        lock(&self.inner.sessions).clone()
    }

    pub async fn task_command_gate(&self, task_id: &str) -> OwnedMutexGuard<()> {
        self.task_gate(task_id).lock_owned().await
    }

    fn queue_drain_gate(&self, task_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.inner.queue_drain_gates)
            .entry(task_id.into())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn claim_executing(&self, chat_id: &str, command_id: &str) -> bool {
        let mut executing = lock(&self.inner.executing);
        if executing.contains_key(command_id) {
            return false;
        }
        executing.insert(command_id.into(), chat_id.into());
        true
    }

    /// Read-only admission check. Apply already owns the task-command lane;
    /// preview may acquire/drop this same guard without opening a cold doc.
    pub async fn prepare_workspace_identity_repair(
        &self,
        task_id: &str,
        repair_command_id: Option<&str>,
    ) -> Result<WorkspaceRepairGuard, EngineError> {
        if !self.is_host(task_id) {
            return Err(EngineError::Other(
                "workspace repair requires the task's local owner".into(),
            ));
        }
        let drain = self
            .queue_drain_gate(task_id)
            .try_lock_owned()
            .map_err(|_| EngineError::Other("task queue delivery is in progress".into()))?;
        let sessions = self
            .sessions()
            .ok_or_else(|| EngineError::Other("task runtime is unavailable".into()))?;
        let execution = sessions.prepare_workspace_repair_execution(task_id)?;
        if lock(&self.inner.executing)
            .values()
            .any(|owner| owner == task_id)
        {
            return Err(EngineError::Other(
                "task command execution is in progress".into(),
            ));
        }
        if let Some(handle) = lock(&self.inner.handles).get(task_id).cloned() {
            if !handle.doc.read_queue()?.is_empty() {
                return Err(EngineError::Other(
                    "task has queued or uncertain messages".into(),
                ));
            }
            if handle
                .doc
                .read_commands()?
                .iter()
                .any(|row| row.status == SessionCommandStatus::Pending)
            {
                return Err(EngineError::Other(
                    "task has pending or executing commands".into(),
                ));
            }
        }
        if let Some(blocker) = self
            .inner
            .store
            .workspace_repair_blocker(task_id, repair_command_id)?
        {
            return Err(EngineError::Other(blocker));
        }
        Ok(WorkspaceRepairGuard {
            _drain: drain,
            _execution: execution,
        })
    }

    fn task_gate(&self, task_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let gate = lock(&self.inner.task_command_gates)
            .entry(task_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        gate
    }

    /// Recovery is opportunistic: leave an active command's task untouched
    /// and revisit it on the next bounded sweep.
    pub(crate) fn try_task_command_gate(&self, task_id: &str) -> Option<OwnedMutexGuard<()>> {
        self.task_gate(task_id).try_lock_owned().ok()
    }

    pub fn begin_task_command(
        &self,
        receipt: CommandReceipt,
        expected_revision: Option<u64>,
        expected_binding_generation: Option<u32>,
        expected_turn_id: Option<&str>,
    ) -> Result<TaskCommandBegin, EngineError> {
        Ok(self.inner.store.begin_task_command(
            receipt,
            expected_revision,
            expected_binding_generation,
            expected_turn_id,
        )?)
    }

    pub fn task_command_receipt(
        &self,
        command_id: &str,
    ) -> Result<Option<CommandReceipt>, EngineError> {
        Ok(self.inner.store.task_command_receipt(command_id)?)
    }

    pub fn context_control(&self, chat_id: &str) -> Result<ContextControlState, EngineError> {
        Ok(self.open(chat_id)?.doc().context_control())
    }

    pub fn persist_goal_readback(
        &self,
        chat_id: &str,
        event_id: &str,
        goal: &luna_proto::GoalReadback,
    ) -> Result<(), EngineError> {
        let handle = self.open(chat_id)?;
        let goal = goal.clone();
        self.persist_queue_projection(&handle, event_id, move |doc| {
            let mut state = doc.context_control();
            if goal.supersedes(state.goal.as_ref()) {
                state.goal = Some(goal);
            }
            doc.set_context_control(&state)?;
            Ok(())
        })?;
        handle.publish_messages();
        Ok(())
    }

    /// Install native goal readback and its receipt atomically without replacing
    /// other provider control state that changed while the RPC was in flight.
    pub fn complete_goal_operation(
        &self,
        chat_id: &str,
        command_id: &str,
        goal: &luna_proto::GoalReadback,
    ) -> Result<CommandReceipt, EngineError> {
        let handle = self.open(chat_id)?;
        let _projection = lock(&handle.queue_projection_lock);
        let goal = goal.clone();
        let prepared = self.prepare_queue_projection(&handle, command_id, move |doc| {
            let mut state = doc.context_control();
            if goal.supersedes(state.goal.as_ref()) {
                state.goal = Some(goal);
            }
            doc.set_context_control(&state)?;
            Ok(())
        })?;
        let pending = self
            .inner
            .store
            .task_command_receipt(command_id)?
            .ok_or_else(|| EngineError::Other("task command receipt vanished".into()))?;
        let snapshots = [(handle.chat_id.as_str(), prepared.snapshot.as_slice())];
        let updates = Self::queue_projection_updates(&handle, &prepared);
        let receipt = self
            .inner
            .store
            .complete_task_command_with_snapshot_changes_updates_and_authority(
                command_id,
                CommandReceiptStatus::Applied,
                true,
                pending.snapshot.binding_id.as_deref(),
                pending.snapshot.binding_generation,
                pending.snapshot.active_turn_id.as_deref(),
                None,
                None,
                &snapshots,
                &[],
                &updates,
                &prepared.authority_changes,
            )?
            .ok_or_else(|| EngineError::Other("task command receipt vanished".into()))?;
        self.install_queue_projection(&handle, prepared)?;
        handle.publish_messages();
        Ok(receipt)
    }

    /// Persist a non-terminal context operation marker before contacting the
    /// provider. A crash after this point is conservatively delivery-unknown;
    /// the operation is never replayed from the pending receipt.
    pub fn persist_context_control(
        &self,
        chat_id: &str,
        operation_id: &str,
        state: &ContextControlState,
    ) -> Result<(), EngineError> {
        let handle = self.open(chat_id)?;
        let state = state.clone();
        self.persist_queue_projection(&handle, operation_id, move |doc| {
            doc.set_context_control(&state)?;
            Ok(())
        })?;
        handle.publish_messages();
        Ok(())
    }

    /// Persist a provider-reported context lifecycle without touching the
    /// task-command receipt store. Automatic provider compaction is not a
    /// user command and must never manufacture acknowledgement semantics.
    pub fn persist_provider_context_control(
        &self,
        chat_id: &str,
        event_id: &str,
        state: &ContextControlState,
        clear_usage: bool,
    ) -> Result<(), EngineError> {
        let handle = self.open(chat_id)?;
        let state = state.clone();
        self.persist_queue_projection(&handle, event_id, move |doc| {
            doc.set_context_control(&state)?;
            if clear_usage {
                doc.clear_context_usage()?;
            }
            Ok(())
        })?;
        handle.publish_messages();
        Ok(())
    }

    /// Finish a compact command with its durable context snapshot and receipt
    /// in one transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn complete_context_operation(
        &self,
        chat_id: &str,
        command_id: &str,
        status: CommandReceiptStatus,
        applied: bool,
        reason: Option<CommandReceiptReason>,
        recovery_action: Option<&str>,
        state: &ContextControlState,
        clear_usage: bool,
    ) -> Result<CommandReceipt, EngineError> {
        let handle = self.open(chat_id)?;
        let _projection = lock(&handle.queue_projection_lock);
        let state = state.clone();
        let prepared = self.prepare_queue_projection(&handle, command_id, move |doc| {
            doc.set_context_control(&state)?;
            if clear_usage {
                doc.clear_context_usage()?;
            }
            Ok(())
        })?;
        let pending = self
            .inner
            .store
            .task_command_receipt(command_id)?
            .ok_or_else(|| EngineError::Other("task command receipt vanished".into()))?;
        let snapshots = [(handle.chat_id.as_str(), prepared.snapshot.as_slice())];
        let updates = Self::queue_projection_updates(&handle, &prepared);
        let receipt = self
            .inner
            .store
            .complete_task_command_with_snapshot_changes_updates_and_authority(
                command_id,
                status,
                applied,
                pending.snapshot.binding_id.as_deref(),
                pending.snapshot.binding_generation,
                pending.snapshot.active_turn_id.as_deref(),
                reason,
                recovery_action,
                &snapshots,
                &[],
                &updates,
                &prepared.authority_changes,
            )?
            .ok_or_else(|| EngineError::Other("task command receipt vanished".into()))?;
        self.install_queue_projection(&handle, prepared)?;
        handle.publish_messages();
        Ok(receipt)
    }

    /// Commit a clear/provider-switch boundary across task identity, registry,
    /// transcript/context state, recovery, and receipt.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_context_boundary<R>(
        &self,
        chat_id: &str,
        command_id: &str,
        binding_id: Option<&str>,
        binding_generation: Option<u32>,
        identity_snapshot: Option<(&str, &[u8])>,
        recovery: Option<&TaskRecoveryTransition>,
        extra_snapshots: &[(&str, &[u8])],
        locally_issued_command_ids: &[&str],
        doc_mutation: impl FnOnce(&SessionDoc) -> Result<R, EngineError>,
        registry_mutation: impl FnOnce(&mut RegistryDoc) -> Result<(), EngineError>,
    ) -> Result<(R, CommandReceipt), crate::workspace_host::TaskProjectionError> {
        let handle = self
            .open(chat_id)
            .map_err(crate::workspace_host::TaskProjectionError::Mutation)?;
        let _projection = lock(&handle.queue_projection_lock);
        let mut prepared = self
            .prepare_queue_projection(&handle, command_id, doc_mutation)
            .map_err(crate::workspace_host::TaskProjectionError::Mutation)?;
        // Newly issued commands have no receipt yet. Inherit the accepted
        // parent's W; the authority insert rechecks it in the commit transaction.
        let admitted_w = if locally_issued_command_ids.is_empty() {
            None
        } else {
            let admitted_w = self
                .inner
                .store
                .command_workspace_repair_revision(command_id)
                .map_err(|error| crate::workspace_host::TaskProjectionError::Commit(error.into()))?
                .ok_or_else(|| {
                    crate::workspace_host::TaskProjectionError::Commit(EngineError::Other(
                        "accepted context command has no workspace repair stamp".into(),
                    ))
                })?;
            let current_w = self
                .inner
                .store
                .workspace_repair_revision(chat_id)
                .map_err(|error| {
                    crate::workspace_host::TaskProjectionError::Commit(error.into())
                })?;
            if admitted_w != current_w {
                return Err(crate::workspace_host::TaskProjectionError::Commit(
                    EngineError::Other(
                        "workspace association changed since context command acceptance".into(),
                    ),
                ));
            }
            Some(admitted_w)
        };
        for issued_command_id in locally_issued_command_ids {
            let position = prepared
                .unreceipted_new_commands
                .iter()
                .position(|(id, _)| id == issued_command_id)
                .ok_or_else(|| {
                    crate::workspace_host::TaskProjectionError::Mutation(EngineError::Other(
                        format!("local context command {issued_command_id} was not newly issued"),
                    ))
                })?;
            let (row_id, hash) = prepared.unreceipted_new_commands.remove(position);
            prepared.authority_changes.push(LocalExecutableChange {
                doc_id: handle.chat_id.clone(),
                kind: LocalExecutableKind::Command,
                row_id,
                expected_hash: None,
                replacement_hash: Some(hash),
                expected_workspace_repair_revision: None,
                replacement_workspace_repair_revision: admitted_w,
            });
        }
        let mut snapshots = vec![(handle.chat_id.as_str(), prepared.snapshot.as_slice())];
        snapshots.extend_from_slice(extra_snapshots);
        let updates = Self::queue_projection_updates(&handle, &prepared);
        let (_, receipt) = self
            .workspace()
            .ok_or_else(|| {
                crate::workspace_host::TaskProjectionError::Mutation(EngineError::Other(
                    "workspace host is unavailable".into(),
                ))
            })?
            .commit_context_boundary(
                command_id,
                binding_id,
                binding_generation,
                identity_snapshot,
                &snapshots,
                &updates,
                recovery,
                &prepared.authority_changes,
                registry_mutation,
            )?;
        let outcome = self
            .install_queue_projection(&handle, prepared)
            .map_err(crate::workspace_host::TaskProjectionError::Commit)?;
        handle.publish_messages();
        handle.publish_queue();
        Ok((outcome, receipt))
    }

    pub(crate) fn task_command_receipt_for_correlation(
        &self,
        task_id: &str,
        correlation_id: &str,
    ) -> Result<Option<CommandReceipt>, EngineError> {
        Ok(self
            .inner
            .store
            .task_command_receipt_for_correlation(task_id, correlation_id)?)
    }

    pub fn task_command_snapshot(&self, task_id: &str) -> Result<TaskCommandRead, EngineError> {
        Ok(self.inner.store.task_command_snapshot(task_id)?)
    }

    pub fn read_task_commands(
        &self,
        task_id: &str,
        after_task_sequence: u64,
        limit: usize,
    ) -> Result<TaskCommandRead, EngineError> {
        Ok(self
            .inner
            .store
            .read_task_commands(task_id, after_task_sequence, limit)?)
    }

    pub fn subscribe_task_receipts(&self) -> broadcast::Receiver<CommandReceipt> {
        self.inner.task_receipts.subscribe()
    }

    pub fn publish_task_receipt(&self, receipt: CommandReceipt) {
        let _ = self.inner.task_receipts.send(receipt);
    }

    #[allow(clippy::too_many_arguments)] // one atomic receipt-finalization boundary
    pub fn complete_task_command(
        &self,
        command_id: &str,
        status: CommandReceiptStatus,
        applied: bool,
        binding_id: Option<&str>,
        binding_generation: Option<u32>,
        active_turn_id: Option<&str>,
        reason: Option<CommandReceiptReason>,
        recovery_action: Option<&str>,
    ) -> Result<Option<CommandReceipt>, EngineError> {
        let receipt = self.inner.store.complete_task_command(
            command_id,
            status,
            applied,
            binding_id,
            binding_generation,
            active_turn_id,
            reason,
            recovery_action,
        )?;
        if let Some(receipt) = receipt.clone() {
            self.publish_task_receipt(receipt);
        }
        Ok(receipt)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn complete_task_command_with_recovery(
        &self,
        command_id: &str,
        status: CommandReceiptStatus,
        applied: bool,
        binding_id: Option<&str>,
        binding_generation: Option<u32>,
        active_turn_id: Option<&str>,
        reason: Option<CommandReceiptReason>,
        recovery_action: Option<&str>,
        recovery: &TaskRecoveryTransition,
    ) -> Result<Option<CommandReceipt>, EngineError> {
        let receipt = self.inner.store.complete_task_command_with_recovery(
            command_id,
            status,
            applied,
            binding_id,
            binding_generation,
            active_turn_id,
            reason,
            recovery_action,
            recovery,
        )?;
        if let Some(receipt) = receipt.clone() {
            self.publish_task_receipt(receipt);
        }
        Ok(receipt)
    }

    pub fn mark_task_command_unknown(
        &self,
        command_id: &str,
        message: &str,
    ) -> Result<Option<CommandReceipt>, EngineError> {
        let receipt = self
            .inner
            .store
            .mark_task_command_unknown(command_id, message)?;
        if let Some(receipt) = receipt.clone() {
            self.publish_task_receipt(receipt);
        }
        Ok(receipt)
    }

    pub fn set_task_command_correlation(
        &self,
        command_id: &str,
        correlation_id: &str,
    ) -> Result<Option<CommandReceipt>, EngineError> {
        let receipt = self
            .inner
            .store
            .set_task_command_correlation(command_id, correlation_id)?;
        if let Some(receipt) = receipt.clone() {
            self.publish_task_receipt(receipt);
        }
        Ok(receipt)
    }

    pub fn set_task_command_correlation_with_recovery(
        &self,
        command_id: &str,
        correlation_id: &str,
        recovery: &TaskRecoveryTransition,
    ) -> Result<Option<CommandReceipt>, EngineError> {
        let receipt = self
            .inner
            .store
            .set_task_command_correlation_with_recovery(command_id, correlation_id, recovery)?;
        if let Some(receipt) = receipt.clone() {
            self.publish_task_receipt(receipt);
        }
        Ok(receipt)
    }

    pub fn confirm_task_command_correlation(
        &self,
        task_id: &str,
        correlation_id: &str,
        active_turn_id: &str,
        binding_id: Option<&str>,
        binding_generation: Option<u32>,
    ) -> Result<Vec<CommandReceipt>, EngineError> {
        let handle = self.open(task_id).ok();
        let mut prepared = None;
        let mut _projection = None;
        if let Some(handle) = handle.as_ref() {
            _projection = Some(lock(&handle.queue_projection_lock));
            let candidate = self.prepare_queue_projection(
                handle,
                &format!("correlation:{correlation_id}"),
                |doc| {
                    let command_id = doc
                        .read_queue()?
                        .into_iter()
                        .find(|item| item.id == correlation_id)
                        .and_then(|item| match item.status {
                            QueueMessageStatus::Claimed { command_id, .. }
                            | QueueMessageStatus::SteeringAccepted { command_id, .. }
                            | QueueMessageStatus::DeliveryUnknown { command_id, .. } => {
                                Some(command_id)
                            }
                            QueueMessageStatus::Held => None,
                        });
                    let Some(command_id) = command_id else {
                        return Ok(false);
                    };
                    let matching_receipt = self
                        .inner
                        .store
                        .task_command_receipt(&command_id)?
                        .is_some_and(|receipt| {
                            receipt.task_id == task_id
                                && receipt.status == CommandReceiptStatus::Pending
                                && receipt.correlation_id.as_deref() == Some(correlation_id)
                                && matches!(
                                    receipt.intent.as_str(),
                                    "sendQueuedNow" | "steerQueuedNow" | "dispatchQueued"
                                )
                        });
                    if !matching_receipt {
                        return Ok(false);
                    }
                    let changed = doc.settle_queued_claim(correlation_id, &command_id)?;
                    Ok(changed)
                },
            )?;
            if candidate.outcome {
                prepared = Some(candidate);
            }
        }
        let snapshots = prepared
            .as_ref()
            .zip(handle.as_ref())
            .map(|(prepared, handle)| vec![(handle.chat_id.as_str(), prepared.snapshot.as_slice())])
            .unwrap_or_default();
        let updates = prepared
            .as_ref()
            .zip(handle.as_ref())
            .map(|(prepared, handle)| Self::queue_projection_updates(handle, prepared))
            .unwrap_or_default();
        let authority = prepared
            .as_ref()
            .map(|prepared| prepared.authority_changes.as_slice())
            .unwrap_or(&[]);
        let receipts = self
            .inner
            .store
            .confirm_task_command_correlation_with_snapshot_changes_and_authority(
                task_id,
                correlation_id,
                active_turn_id,
                binding_id,
                binding_generation,
                &snapshots,
                &updates,
                authority,
            )?;
        if !receipts.is_empty()
            && let (Some(handle), Some(prepared)) = (handle.as_ref(), prepared)
        {
            self.install_queue_projection(handle, prepared)?;
        }
        for receipt in &receipts {
            self.publish_task_receipt(receipt.clone());
        }
        Ok(receipts)
    }

    /// Record a provider startup the adapter PROVED was rejected before any
    /// user turn was accepted. Rejects only the exact uncommitted pending `run`
    /// receipt for `correlation_id` under the workspace-repair fence and the
    /// live binding-generation fence (the same fence command execution uses),
    /// and marks this device's frozen submission recoverable in the same
    /// transaction. Returns the receipts it rejected; an empty result means the
    /// receipt was already terminal, committed-unknown, accepted, of another
    /// intent or fenced out, and nothing was changed.
    pub fn reject_run_before_acceptance(
        &self,
        task_id: &str,
        correlation_id: &str,
        expected_workspace_repair_revision: u64,
        message: &str,
    ) -> Result<Vec<CommandReceipt>, EngineError> {
        let binding_generation = self
            .sessions()
            .and_then(|sessions| sessions.task_binding_generation(task_id));
        let receipts = self.inner.store.reject_run_before_acceptance(
            self.device_id(),
            task_id,
            correlation_id,
            expected_workspace_repair_revision,
            binding_generation,
            message,
            "The provider did not start this message, so nothing was sent. Review the preserved input and send it again.",
        )?;
        for receipt in &receipts {
            self.publish_task_receipt(receipt.clone());
        }
        Ok(receipts)
    }

    /// Wire the sessions engine (engine assembly; see `SessionsEngine::set_doc_host`).
    pub fn set_sessions(&self, sessions: SessionsEngine) {
        let statuses = sessions.watch_sessions();
        {
            // First set wins (the OnceLock contract this slot replaced).
            let mut slot = lock(&self.inner.sessions);
            if slot.is_none() {
                *slot = Some(sessions);
            }
        }
        // Commands may already be pending in warm-opened docs.
        let handles: Vec<_> = lock(&self.inner.handles).values().cloned().collect();
        for handle in handles {
            let host = self.clone();
            self.spawn_worker(async move {
                host.drain_commands(&handle).await;
                host.drain_queue(&handle).await;
            });
        }
        self.spawn_queue_flush_watcher(statuses);
    }

    /// The turn-end hook for held messages. Doc changes drive `drain_queue` for
    /// everything else, but a turn ENDING is not a doc change the queue can
    /// see — so watch session status instead and re-drain every warm chat.
    /// `drain_queue` is a cheap no-op for empty queues and busy agents, which
    /// is why this can afford to be indiscriminate.
    fn spawn_queue_flush_watcher(&self, mut statuses: watch::Receiver<Vec<luna_proto::Session>>) {
        let host = self.clone();
        self.spawn_worker(async move {
            while statuses.changed().await.is_ok() {
                let handles: Vec<_> = lock(&host.inner.handles).values().cloned().collect();
                for handle in handles {
                    host.drain_queue(&handle).await;
                }
            }
        });
    }

    /// Retire this host's workers (runtime replacement, e.g. sign-out): cancel
    /// and await every spawned task, drop every open chat handle (ending the
    /// weak-keyed room/join loops and watcher streams), and sever the sessions
    /// back-edge so the replaced engine graph can actually drop. Idempotent.
    pub async fn shutdown_workers(&self) {
        self.inner.shutdown.cancel();
        self.inner.tasks.close();
        self.inner.tasks.wait().await;
        // Snapshot open docs BEFORE releasing their handles: the handles map
        // holds the only strong doc refs, and an unflushed doc dies with it.
        self.flush_all();
        // Take the map under the lock, drop the handles outside it.
        let handles = std::mem::take(&mut *lock(&self.inner.handles));
        drop(handles);
        lock(&self.inner.seeding).clear();
        lock(&self.inner.seed_waiting).clear();
        lock(&self.inner.sessions).take();
    }

    /// Stop workers and retain open docs if a final snapshot cannot be
    /// persisted. A caller may repair the storage error and retry the flush.
    pub(crate) async fn shutdown_workers_checked(&self) -> Result<(), EngineError> {
        self.inner.shutdown.cancel();
        self.inner.tasks.close();
        self.inner.tasks.wait().await;
        self.flush_all_checked()?;
        let handles = std::mem::take(&mut *lock(&self.inner.handles));
        drop(handles);
        lock(&self.inner.seeding).clear();
        lock(&self.inner.seed_waiting).clear();
        lock(&self.inner.sessions).take();
        Ok(())
    }

    /// Freeze every open queue before settling live runs during shutdown.
    /// Interrupting a run publishes Idle, which normally wakes the turn-end
    /// queue drainer; without this barrier quitting the host could promote a
    /// queued row in the narrow window before workers are retired.
    pub fn pause_all_queues(&self) {
        let handles: Vec<_> = lock(&self.inner.handles).values().cloned().collect();
        for handle in handles {
            handle.queue_paused.store(true, Ordering::Release);
        }
    }

    /// Acceptance-time stop fence. The durable interrupt command may execute
    /// on the session-doc worker a moment later; setting this under the same
    /// drain lock prevents an already-accepted Stop from losing to a late
    /// upload/drain/send-now callback in that window. Returns the prior pause
    /// state so a local storage rejection can restore it.
    pub async fn fence_queue_for_stop(&self, chat_id: &str) -> Result<bool, EngineError> {
        let handle = self.open(chat_id)?;
        let _drain = handle.drain_lock.lock().await;
        Ok(handle.queue_paused.swap(true, Ordering::AcqRel))
    }

    pub fn restore_queue_pause_after_failed_stop(
        &self,
        chat_id: &str,
        was_paused: bool,
    ) -> Result<(), EngineError> {
        let handle = self.open(chat_id)?;
        handle.queue_paused.store(was_paused, Ordering::Release);
        Ok(())
    }

    /// Test-only retirement sentinel: reports true once the doc-host graph
    /// has actually been freed.
    #[doc(hidden)]
    pub fn retirement_probe(&self) -> Box<dyn Fn() -> bool + Send + Sync> {
        let weak = Arc::downgrade(&self.inner);
        Box::new(move || weak.upgrade().is_none())
    }

    /// Wire the repos engine (engine assembly) — worktree materialization for
    /// Run commands carrying a [`luna_proto::WorktreeSpec`].
    pub fn set_project_commands(&self, commands: crate::project_commands::ProjectCommands) {
        let _ = self.inner.project_commands.set(commands);
    }

    pub fn set_repos(&self, repos: crate::repos::Repos) {
        let _ = self.inner.repos.set(repos);
    }

    /// Wire the uploads store (engine assembly) — `pending://` ref resolution
    /// and the transfer-read jail.
    pub fn set_uploads(&self, uploads: crate::uploads::Uploads) {
        let _ = self.inner.uploads.set(uploads);
    }

    /// Wire the peer-link cache (engine assembly, edge runtimes only) — the
    /// transport for queued attachment transfers to a remote host.
    pub fn set_links(&self, links: Arc<luna_rpc::LinkCache>) {
        let _ = self.inner.links.set(links);
    }

    /// Re-evaluate every open chat's command queue NOW. Called after an
    /// upload commit lands bytes on this device: a Run deferred on those
    /// bytes (`pending://` refs not yet on disk) becomes executable the
    /// moment its transfer completes — event-driven, not timer luck.
    pub fn kick_drains(&self) {
        let handles: Vec<Arc<ChatDocHandle>> =
            lock(&self.inner.handles).values().cloned().collect();
        for handle in handles {
            let host = self.clone();
            self.spawn_worker(async move { host.drain_commands(&handle).await });
        }
    }

    /// Wire the workspace host (engine assembly) — the source of chat-ownership rows.
    pub fn set_workspace(&self, workspace: WorkspaceHost) {
        let chats = workspace.watch_chats();
        if self.inner.workspace.set(workspace).is_ok() {
            self.spawn_cutover_watcher(chats);
            self.spawn_migration_sweep();
        }
    }

    /// Host migration sweep: proactively seed this device's own s2 chats
    /// onto chat2, one per tick. The lazy open()-triggered seed could never
    /// fire in real usage — idle chats are never opened, a headless host's
    /// only opens are nudges (which arrive exactly when a run starts), and
    /// viewing a chat used to pin it. Every safety gate stays (s2 room
    /// joined, 2s frontier quiet, no live writer, no pending commands,
    /// frontier seal at the flip) — only the trigger changes: the host owns
    /// migrating its chats. Aborted seeds retry after a gap; a migrated
    /// fleet makes the tick a cheap no-op scan.
    fn spawn_migration_sweep(&self) {
        const TICK: std::time::Duration = std::time::Duration::from_secs(30);
        const RETRY_GAP_MS: i64 = 10 * 60 * 1000;
        let host = self.clone();
        self.spawn_worker(async move {
            let mut attempted: HashMap<String, i64> = HashMap::new();
            loop {
                tokio::time::sleep(TICK).await;
                let Some(edge) = host.inner.config.edge.clone() else {
                    return; // edge-less engine: nothing to migrate onto
                };
                let Some(ws) = host.workspace() else { continue };
                let chats: Vec<luna_proto::Chat> = ws.watch_chats().borrow().clone();
                let device = host.inner.config.device_id.clone();
                let now = now_ms();
                let candidate = chats.into_iter().find(|c| {
                    !host.inner.store.is_side_chat(&c.id).unwrap_or(true)
                        && c.device_id == device
                        && c.room_gen.unwrap_or(1) < 2
                        && !attempted
                            .get(&c.id)
                            .is_some_and(|t| now - *t <= RETRY_GAP_MS)
                });
                let Some(chat) = candidate else { continue };
                attempted.insert(chat.id.clone(), now);
                // A cached (warm) handle never re-enters open()'s build path,
                // so arm the quiet-waiter directly; a cold chat opens, which
                // arms it on the way up.
                let cached = lock(&host.inner.handles).get(&chat.id).cloned();
                match cached {
                    Some(handle) => {
                        if handle.room_gen < 2 && !handle.retired.load(Ordering::Relaxed) {
                            tracing::debug!(chat = %chat.id, "migration sweep: arming seed on warm s2 chat");
                            host.spawn_chat2_seed_when_quiet(edge, &chat.id, &handle);
                        }
                    }
                    None => {
                        tracing::debug!(chat = %chat.id, "migration sweep: opening cold s2 chat to seed");
                        if let Err(err) = host.open(&chat.id) {
                            tracing::warn!(chat = %chat.id, error = %err, "migration sweep open failed");
                        }
                    }
                }
            }
        });
    }

    /// Live cutover convergence: when a registry chat row flips to roomGen 2
    /// while this device holds an s2-mode handle, retire it NOW — a viewer
    /// watching the chat at flip time otherwise stays frozen on the dead s2
    /// room until they happen to reopen (the host writes only to chat2 from
    /// the flip on). Dropping the handle ends the watch streams cleanly; the
    /// UI's transcript/standing watches resubscribe and the fresh open takes
    /// the chat2 adopt path. A handle with a LIVE local writer (a running
    /// turn's doc ref) is left alone — the host never flips mid-run, and a
    /// racing writer must never lose its doc out from under it.
    fn spawn_cutover_watcher(&self, mut chats: watch::Receiver<Vec<luna_proto::Chat>>) {
        let host = self.clone();
        self.spawn_worker(async move {
            loop {
                if chats.changed().await.is_err() {
                    return; // workspace host gone (shutdown)
                }
                let flipped: Vec<String> = chats
                    .borrow_and_update()
                    .iter()
                    .filter(|c| c.room_gen.unwrap_or(1) >= 2)
                    .map(|c| c.id.clone())
                    .collect();
                if flipped.is_empty() {
                    continue;
                }
                let mut dropped: Vec<String> = Vec::new();
                let mut stuck_live: Vec<(String, Arc<ChatDocHandle>)> = Vec::new();
                {
                    let mut handles = lock(&host.inner.handles);
                    for chat_id in flipped {
                        let Some(handle) = handles.get(&chat_id) else {
                            continue;
                        };
                        if handle.room_gen >= 2 {
                            continue; // already chat2-mode
                        }
                        let live_writer = Arc::strong_count(&handle.doc) > 1;
                        if live_writer {
                            // A run is writing into this s2 doc while the
                            // registry already says chat2 — the born-gen2
                            // race (this open beat its own CreateChat mint,
                            // 2026-08-11: transcript never reached any other
                            // device). No thin lineage exists on disk yet,
                            // so retiring here would suppress the doc's only
                            // persistence and abort the quiet-seed. Leave it
                            // live; seed once the run quiesces — the seed
                            // posts the chat2 checkpoint, persists the thin
                            // lineage, and retires the handle itself.
                            stuck_live.push((chat_id, handle.clone()));
                            continue;
                        }
                        handle.retired.store(true, Ordering::Relaxed);
                        handles.remove(&chat_id);
                        tracing::info!(chat = %chat_id,
                            "s2 handle dropped on chat2 cutover; watchers resubscribe onto the new room");
                        dropped.push(chat_id);
                    }
                }
                for (chat_id, handle) in stuck_live {
                    if host.is_host(&chat_id)
                        && let Some(edge) = host.inner.config.edge.clone()
                    {
                        host.spawn_chat2_seed_when_quiet(edge, &chat_id, &handle);
                    }
                }
                // Watchers resubscribe on their own — but a NUDGE-opened
                // handle has none, and its s2 room never carried the queued
                // command anyway (the sender pushed to chat2). On a born-
                // chat2 chat the nudge beats the registry row by design
                // (direct HTTP vs room sync), so the first open lands here
                // and dying silently strands the first message until the
                // next nudge — every new remote session's first send sat
                // ~30s+ until the user re-sent (user report). If we host
                // the chat, reopen NOW: the fresh open dials the chat2
                // room and the change-driven drain executes the command.
                for chat_id in dropped {
                    if !host.is_host(&chat_id) {
                        continue;
                    }
                    match host.open(&chat_id) {
                        Ok(_) => tracing::info!(chat = %chat_id,
                            "reopened as chat2 after cutover drop (host, pending work possible)"),
                        Err(err) => tracing::warn!(chat = %chat_id, error = %err,
                            "chat2 reopen after cutover drop failed"),
                    }
                }
            }
        });
    }

    /// The workspace host, once wired (tests may assemble a DocHost without one).
    pub fn workspace(&self) -> Option<&WorkspaceHost> {
        self.inner.workspace.get()
    }

    pub(crate) fn default_harness(&self) -> HarnessId {
        self.inner.config.default_harness
    }

    pub fn device_id(&self) -> &str {
        &self.inner.config.device_id
    }

    /// Open (or return) the chat's doc handle: load the local snapshot (or init fresh),
    /// start the change-driven task, and join the edge room when configured.
    /// Side transcripts and their native subagent docs belong to one profile.
    /// The replicated marker also fails closed when local history is absent.
    pub(crate) fn is_side_document(&self, chat_id: &str) -> bool {
        let owner = chat_id
            .split_once("--sub--")
            .map_or(chat_id, |(owner, _)| owner);
        self.inner.store.is_side_chat(owner).unwrap_or(true)
            || self
                .workspace()
                .is_some_and(|workspace| workspace.is_side_chat(owner))
    }

    pub fn open(&self, chat_id: &str) -> Result<Arc<ChatDocHandle>, EngineError> {
        let local_only = self.is_side_document(chat_id);
        let side_owner = chat_id
            .split_once("--sub--")
            .map_or(chat_id, |(owner, _)| owner);
        if self
            .workspace()
            .is_some_and(|workspace| workspace.is_side_chat(side_owner))
            && !self.inner.store.is_side_chat(side_owner)?
        {
            return Err(EngineError::Other(
                "Side chat history is available only in its owning profile".into(),
            ));
        }
        // The registry names the sync room generation (docs/chat2-sync.md
        // M2): absent row / absent field = legacy s2. Read it BEFORE the
        // cached-handle check — a cached s2-mode handle for a chat another
        // device has since cut over to chat2 would otherwise serve its frozen
        // fat lineage forever (the host writes only to the chat2 room now;
        // this device's s2 room has gone permanently silent).
        let chat_row = self
            .workspace()
            .and_then(|w| w.chat(chat_id).ok().flatten());
        // A row that EXISTS without `roomGen` is a pre-cutover legacy chat
        // (gen 1). A MISSING row is a chat being born right now: its
        // CreateChat mint (which stamps roomGen 2) is racing this open —
        // the composer attaches the transcript watch before its own mutate
        // lands, and a nudge beats registry sync by design. Defaulting the
        // absent row to 1 minted brand-new s2 rooms post-cutover: the host
        // ran the whole session against a room no other device reads (they
        // follow the row's gen 2 to an empty chat2 room), the run's live doc
        // ref blocked every heal, and the transcript never synced anywhere
        // (2026-08-11).
        let registry_gen = match chat_row.as_ref() {
            Some(row) => row.room_gen.unwrap_or(1),
            None => 2,
        };
        {
            let mut handles = lock(&self.inner.handles);
            if let Some(handle) = handles.get(chat_id) {
                if handle
                    .durable_projection_uninstalled
                    .load(Ordering::Acquire)
                {
                    return Err(EngineError::Other(
                        "a committed document projection needs recovery after restart".into(),
                    ));
                }
                let stale = registry_gen >= 2 && handle.room_gen < 2;
                if (stale || handle.retired.load(Ordering::Relaxed)) && !self.pinned(handle) {
                    // A seed flipped this chat under a cached fat handle
                    // (review B1): drop it so this open converges onto the
                    // thin lineage + chat2 room. Retire only at the drop:
                    // marking a PINNED stale handle retired while it kept
                    // serving suppressed the only persistence a stuck-live
                    // doc had and aborted its quiet-seed (born-gen2 race,
                    // 2026-08-11) — the cutover watcher and the seed itself
                    // own converging pinned handles.
                    handle.retired.store(true, Ordering::Relaxed);
                    handles.remove(chat_id);
                } else {
                    handle.touch();
                    return Ok(handle.clone());
                }
            }
        }
        let opening = lock(&self.inner.opening);
        if let Some(handle) = lock(&self.inner.handles).get(chat_id) {
            if handle
                .durable_projection_uninstalled
                .load(Ordering::Acquire)
            {
                return Err(EngineError::Other(
                    "a committed document projection needs recovery after restart".into(),
                ));
            }
            handle.touch();
            return Ok(handle.clone());
        }
        // B2/M5 guard: the LOCAL epoch is the second cutover signal. A crash
        // between the thin save and the registry flip (or a not-yet-synced
        // registry) must NOT route an epoch-2 doc back onto s2 — the s2
        // room's fat doc would merge into the unrelated thin lineage and
        // duplicate every message. Local epoch >= 2 forces the chat2 branch
        // and best-effort completes the flip.
        let stored = self.inner.store.load_snapshot_with_cursor(chat_id)?;
        let stored_epoch = stored.as_ref().map(|(_, _, e)| *e).unwrap_or(0);
        let room_gen = if stored_epoch >= crate::chat2_host::CHAT2_DOC_EPOCH {
            if registry_gen < 2
                && let Some(ws) = self.workspace()
            {
                let _ = ws.set_chat_room_gen(chat_id, 2);
                tracing::info!(chat = %chat_id,
                    "completed interrupted chat2 flip (local epoch 2, registry said s2)");
            }
            2
        } else {
            registry_gen
        };
        let mut snapshot_len = 0usize;
        let mut chat2_cursor = 0u64;
        let mut requeue_commands: Vec<SessionCommandEntry> = Vec::new();
        let doc = if room_gen >= 2 {
            match stored {
                Some((bytes, cursor, epoch)) if epoch >= crate::chat2_host::CHAT2_DOC_EPOCH => {
                    snapshot_len = bytes.len();
                    chat2_cursor = cursor;
                    let raw = loro::LoroDoc::new();
                    raw.import(&bytes)
                        .map_err(|e| EngineError::Other(format!("snapshot import failed: {e}")))?;
                    SessionDoc::from_doc(raw)
                }
                Some((bytes, _cursor, epoch)) if local_only || self.inner.config.edge.is_none() => {
                    // Offline/edge-less: adopting would blank a readable
                    // transcript with no way to catch up (review B4). Keep
                    // the old doc read-only-ish; the adopt runs on the next
                    // online open.
                    tracing::info!(chat = %chat_id, old_epoch = epoch,
                        "chat2 adopt deferred (no edge configured)");
                    snapshot_len = bytes.len();
                    let raw = loro::LoroDoc::new();
                    raw.import(&bytes)
                        .map_err(|e| EngineError::Other(format!("snapshot import failed: {e}")))?;
                    SessionDoc::from_doc(raw)
                }
                Some((bytes, _cursor, epoch)) => {
                    // M3 discard-and-adopt: this device's doc predates the
                    // chat2 lineage. Keep the old snapshot under a suffixed
                    // id for rollback, carry over OUR OWN unresolved
                    // commands, and start fresh — the chat2 catch-up
                    // (checkpoint + rows) repopulates the transcript. This
                    // is the self-repair path: no user action, ever.
                    tracing::info!(chat = %chat_id, old_epoch = epoch,
                        "chat2 adopt: discarding pre-chat2 local doc (rollback copy kept)");
                    let rollback_id = format!("{chat_id}.pre-chat2");
                    // A re-adopt after a mid-catch-up crash reruns this path
                    // with a near-empty doc under `chat_id` — the FIRST
                    // rollback copy is the real transcript; never overwrite
                    // it (review B5).
                    if matches!(self.inner.store.load_snapshot(&rollback_id), Ok(None)) {
                        let _ = self.inner.store.save_snapshot(&rollback_id, &bytes);
                    }
                    if let Ok(raw) = {
                        let old = loro::LoroDoc::new();
                        old.import(&bytes).map(|_| old)
                    } {
                        let old_doc = SessionDoc::from_doc(raw);
                        if let Ok(commands) = old_doc.read_commands() {
                            requeue_commands = commands
                                .into_iter()
                                .filter(|c| {
                                    c.status == SessionCommandStatus::Pending
                                        && c.issued_by == self.inner.config.device_id
                                })
                                .collect();
                        }
                    }
                    SessionDoc::init(chat_id)?
                }
                None => {
                    // Born on chat2 (or a cold reader's first open): stamp
                    // the epoch-2 lineage NOW. Plain snapshot saves preserve
                    // an existing row's epoch but default a NEW row to 0 —
                    // without this stamp, the next open reads "pre-chat2
                    // doc" and the M3 adopt DISCARDS everything written
                    // since (caught by the restart_resume suite: first-turn
                    // transcripts vanished on reopen).
                    let doc = SessionDoc::init(chat_id)?;
                    if let Ok(snapshot) = doc.export_snapshot() {
                        let _ = self.inner.store.save_snapshot_with_cursor(
                            chat_id,
                            &snapshot,
                            0,
                            crate::chat2_host::CHAT2_DOC_EPOCH,
                        );
                    }
                    doc
                }
            }
        } else {
            match stored {
                Some((bytes, _, _)) => {
                    snapshot_len = bytes.len();
                    let raw = loro::LoroDoc::new();
                    raw.import(&bytes)
                        .map_err(|e| EngineError::Other(format!("snapshot import failed: {e}")))?;
                    SessionDoc::from_doc(raw)
                }
                None => SessionDoc::init(chat_id)?,
            }
        };
        // Recover committed outgoing operations even when the snapshot debounce
        // did not run before a crash. Imported updates do not echo as local writes.
        if room_gen >= 2 {
            for (_, bytes) in self.inner.store.pending_chat_updates(chat_id)? {
                doc.doc()
                    .import(&bytes)
                    .map_err(|e| EngineError::Other(e.to_string()))?;
            }
        }
        // Reconcile crash windows before publishing the recovered queue. A
        // terminal applied receipt proves provider acceptance for this exact
        // claim and may finish removing it. Every unresolved/missing receipt
        // stays visible as DeliveryUnknown; startup never replays it.
        let queue_reconcile_frontier = doc.doc().oplog_vv();
        let mut reconciled_queue_claim = false;
        for item in doc.read_queue().unwrap_or_default() {
            let command_id = match &item.status {
                QueueMessageStatus::Claimed { command_id, .. }
                | QueueMessageStatus::SteeringAccepted { command_id, .. }
                | QueueMessageStatus::DeliveryUnknown { command_id, .. } => command_id,
                QueueMessageStatus::Held => continue,
            };
            // Restored/synced rows do not own receipts merely by naming an
            // existing command ID. Both acceptance and uncertainty may only
            // affect the exact task/message delivery operation.
            let receipt = self
                .inner
                .store
                .task_command_receipt(command_id)?
                .filter(|receipt| {
                    receipt.task_id == chat_id
                        && receipt.correlation_id.as_deref() == Some(item.id.as_str())
                        && matches!(
                            receipt.intent.as_str(),
                            "sendQueuedNow" | "steerQueuedNow" | "dispatchQueued"
                        )
                });
            match receipt {
                Some(receipt) if receipt.status == CommandReceiptStatus::Applied => {
                    reconciled_queue_claim |= doc.settle_queued_claim(&item.id, command_id)?;
                }
                receipt => {
                    reconciled_queue_claim |=
                        doc.mark_queued_claim_unknown(&item.id, command_id, now_ms())?;
                    if receipt
                        .is_some_and(|receipt| receipt.status == CommandReceiptStatus::Pending)
                    {
                        let _ = self.mark_task_command_unknown(
                            command_id,
                            "engine restarted with an unresolved queue claim; reconcile provider state before retry",
                        );
                    }
                }
            }
        }
        if reconciled_queue_claim && let Ok(snapshot) = doc.export_snapshot() {
            let update = doc
                .doc()
                .export(loro::ExportMode::updates(&queue_reconcile_frontier))
                .map_err(|error| EngineError::Other(error.to_string()))?;
            let batch_id = format!("queue-reconcile-{}", uuid::Uuid::new_v4());
            let updates = if room_gen >= 2
                && !update.is_empty()
                && self.inner.store.chat_outbox_initialized(chat_id)?
            {
                vec![(batch_id.as_str(), update.as_slice())]
            } else {
                Vec::new()
            };
            self.inner
                .store
                .save_snapshot_with_updates(chat_id, &snapshot, &updates)?;
        }
        let doc = Arc::new(doc);

        let (changed_tx, changed_rx) = watch::channel(0u64);
        let (messages_tx, _) = watch::channel(TranscriptSnapshot::default());
        let transcript_history = Arc::new(Mutex::new(
            crate::transcript_history::TranscriptHistory::default(),
        ));
        let history = transcript_history.clone();
        let watched = messages_tx.clone();
        let weak_doc = Arc::downgrade(&doc);
        let sub = doc.doc().subscribe_root(Arc::new(move |diff| {
            // This callback runs before the change worker can publish. The
            // import origin belongs to the event, so concurrent local commits
            // cannot inherit a remote replay's presentation classification.
            if watched.receiver_count() > 0 {
                if let Some(doc) = weak_doc.upgrade() {
                    lock(&history).observe(doc.doc(), &diff);
                }
            } else {
                *lock(&history) = Default::default();
            }
            changed_tx.send_modify(|v| *v = v.wrapping_add(1));
        }));
        // The mirror starts dirty and empty; watch_messages materializes it
        // once on attach instead of maintaining an unwatched transcript.
        let initial_queue = doc.read_queue().unwrap_or_default();
        // A queue already present when a handle is materialized came from a
        // persisted snapshot (or a synced checkpoint), not from a prompt the
        // user submitted to this live engine. Keep that recovered work frozen
        // until an explicit prompt / Send now / Steer action thaws it. Rows
        // appended after the handle exists retain the normal automatic drain.
        let recovered_queue_pending = !initial_queue.is_empty();
        let (queue_tx, _) = watch::channel(initial_queue);

        let handle = Arc::new(ChatDocHandle {
            chat_id: chat_id.to_string(),
            device_id: self.inner.config.device_id.clone(),
            doc: doc.clone(),
            messages_tx,
            transcript_import: Mutex::default(),
            transcript_history,
            queue_tx,
            drain_lock: self.queue_drain_gate(chat_id),
            queue_projection_lock: Mutex::new(()),
            durable_projection_uninstalled: AtomicBool::new(false),
            queue_paused: AtomicBool::new(recovered_queue_pending),
            mirror_dirty: AtomicBool::new(true),
            last_access: AtomicI64::new(now_ms()),
            snapshot_bytes: AtomicUsize::new(snapshot_len),
            room_gen,
            retired: AtomicBool::new(false),
            checkpointing: Arc::new(AtomicBool::new(false)),
            chat2: Mutex::new(None),
            chat2_pending_local: Mutex::new(Vec::new()),
            publication_failed: AtomicBool::new(false),
            chat2_local_sub: Mutex::new(None),
            _sub: sub,
        });
        // Snapshot recovery may restore several independently edited rows.
        // Each row needs its own checked expiry wake; otherwise a non-head
        // edit could remain displayed as live indefinitely.
        self.arm_existing_queue_edit_expiries(&handle);

        // Edge room join — offline-tolerant AND supervised. `ChatClient` only
        // self-reconnects AFTER a first successful join; a one-shot attempt
        // here (the pre-LRU design) left the doc silently local-only until
        // app restart whenever the dial hit a transient gap — a post-wake
        // network, `Auth::token()` momentarily `None` around a refresh, an
        // edge deploy. The LRU made that dice-roll constant (every reopen),
        // and a watched doc is pinned against eviction, so nothing ever
        // retried: the exact "transcript frozen until restart" report.
        // Retry on the workspace host's capped, jittered backoff; a system
        // wake redials immediately; eviction/purge ends the loop via `weak`.
        if !local_only && let Some(edge) = &self.inner.config.edge {
            if room_gen >= 2 {
                // Subscription BEFORE the dial (review B3): every local
                // commit lands in the client when connected, else in the
                // pending buffer the join drains — nothing composed during
                // (or before) the dial is lost to the room.
                // A one-time full replay heals history stranded by older clients.
                // Its durable marker is independent of the download cursor.
                if !self.inner.store.chat_outbox_initialized(chat_id)? {
                    let updates = crate::chat2_host::publication_updates(doc.doc())
                        .map_err(EngineError::Other)?;
                    self.inner.store.initialize_chat_outbox(chat_id, &updates)?;
                }
                let weak_push = Arc::downgrade(&handle);
                let publication_store = self.inner.store.clone();
                let publication_chat = chat_id.to_string();
                let sub = doc
                    .doc()
                    .subscribe_local_update(Box::new(move |bytes: &Vec<u8>| {
                        if let Some(handle) = weak_push.upgrade() {
                            // The buffer push happens WHILE HOLDING the client
                            // lock (verify pass: releasing it between the None
                            // check and the push let the join's store+drain
                            // slip between, orphaning the update forever).
                            let batch_id = uuid::Uuid::new_v4().to_string();
                            if let Err(err) = publication_store.enqueue_chat_update(&publication_chat, &batch_id, bytes) {
                                handle.publication_failed.store(true, Ordering::Release);
                                tracing::error!(chat = %publication_chat, %err, "chat2: durable outbox write failed");
                            }
                            let client_guard = lock(&handle.chat2);
                            match &*client_guard {
                                Some(client) => client.enqueue_batch(batch_id, bytes.clone()),
                                None => lock(&handle.chat2_pending_local).push((batch_id, bytes.clone())),
                            }
                        }
                        true
                    }));
                *lock(&handle.chat2_local_sub) = Some(sub);
                // Re-queue survives the adopt: our own pending commands
                // become fresh entries in the new lineage (the
                // processed_commands ledger still guards double execution).
                // Committed AFTER the local-update subscription above — a
                // commit before it never enters the pending buffer or the
                // client, so the requeued command would sit in the local doc
                // and never reach the room (the host would never see it).
                for command in &requeue_commands {
                    let _ = doc.queue_command(command);
                }
                if !self.inner.edge_disconnected.load(Ordering::Acquire) {
                    self.spawn_chat2_join(edge.clone(), &handle, chat2_cursor);
                }
            } else {
                // Straggler gen-1 chat (the s2 client is gone — post-cutover,
                // no device reads or writes an s2 room). The local fat doc
                // serves reads as-is; if we host the chat, seed it onto chat2
                // in the background and the flip converges every device.
                let is_host = chat_row
                    .as_ref()
                    .is_some_and(|c| c.device_id == self.inner.config.device_id);
                if is_host && chat_row.is_some() {
                    // Quiescent-only (review B1): a seed under a live run or
                    // watched transcript would strand everything written
                    // after the rebuild instant in a retired fat lineage.
                    self.spawn_chat2_seed_when_quiet(edge.clone(), chat_id, &handle);
                }
            }
        }
        let locally_hosted = chat_row
            .as_ref()
            .is_some_and(|chat| chat.device_id == self.inner.config.device_id);
        if locally_hosted && lock(&self.inner.context_reconciled).insert(chat_id.to_string()) {
            let mut context = doc.context_control();
            if context.compact.status == luna_proto::ContextOperationStatus::Running {
                context.compact.status = luna_proto::ContextOperationStatus::Unknown;
                context.compact.finished_at_ms = Some(now_ms());
                context.compact.detail = Some(
                    "The previous engine stopped before provider compaction completion was durably observed. LUNA will not replay it."
                        .into(),
                );
                doc.set_context_control(&context)?;
            }
        }
        // Publish only after the durable subscription and bootstrap are installed.
        lock(&self.inner.handles).insert(chat_id.to_string(), handle.clone());
        drop(opening);
        self.spawn_worker(chat_task(self.clone(), Arc::downgrade(&handle), changed_rx));
        self.evict_over_budget();
        Ok(handle)
    }

    /// chat2 relay join (docs/chat2-sync.md C3): deadline on every dial,
    /// capped jittered backoff, wake redial — and the client resolves only
    /// after full catch-up (checkpoint + rows), so "joined" here means
    /// "transcript converged".
    fn spawn_chat2_join(&self, edge: EdgeConfig, handle: &Arc<ChatDocHandle>, cursor: u64) {
        if self.is_side_document(&handle.chat_id) {
            return;
        }
        let chat = handle.chat_id.clone();
        let doc = handle.doc.clone();
        let store = self.inner.store.clone();
        let http = self.inner.http.clone();
        let device = self.inner.config.device_id.clone();
        let weak = Arc::downgrade(handle);
        let host = self.clone();
        let mut token_changes = edge.token_changes();
        self.spawn_worker(async move {
            let sink = Arc::new(crate::chat2_host::EngineChatSink::new(&doc, store, chat.clone())
                .with_handle(weak.clone()));
            // The sink holds only a Weak doc ref (a strong one made every
            // chat2 handle read as perma-pinned — LRU eviction dead); this
            // task's own strong ref dies when the join resolves.
            drop(doc);
            let fetcher = Arc::new(crate::chat2_host::EdgeCheckpointFetcher::new(
                http,
                edge.clone(),
                chat.clone(),
            ));
            let url = edge.room_url(format!("/chat2/{chat}/ws"));
            let mut wake = luna_sync::wake::subscribe();
            // Sibling-dial successes end a backoff wait immediately, exactly
            // like the joined clients' own reconnect loops (chat_client.rs).
            // Without this, a NEW chat whose first joins hit a network blip
            // waited out the full accumulated backoff (→30s) while every
            // established room redialed instantly on recovery — fresh sends
            // to new sessions stalled while other chats hummed (2026-08-19
            // user report, reproduced on two networks).
            let mut online = luna_sync::wake::subscribe_online();
            let mut backoff = crate::workspace_host::JOIN_RETRY_BASE;
            loop {
                if weak.upgrade().is_none() {
                    return; // evicted or purged while dialing
                }
                // Dual transport: WS dial + a plain-HTTPS pull/push seam
                // (rows GET / POST on the same bearer auth as the checkpoint
                // fetch) — bootstraps in ~1 RTT and keeps syncing at backoff
                // cadence on networks that never pass the WS upgrade. With
                // the transport, connect resolves immediately (local-first).
                let transport = Arc::new(crate::chat2_host::EdgeChatTransport::new(
                    host.inner.http.clone(),
                    edge.clone(),
                    chat.clone(),
                    device.clone(),
                ));
                let dial = tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    luna_sync::ChatClient::connect_via_transport(
                        url.clone(),
                        sink.clone(),
                        fetcher.clone(),
                        &device,
                        cursor,
                        transport,
                    ),
                )
                .await;
                match dial {
                    Ok(Ok(client)) => {
                        if matches!(edge.bearer().await, Err(luna_rpc::TokenError::SignedOut)) {
                            return;
                        }
                        let Some(handle) = weak.upgrade() else {
                            return; // evicted mid-dial: drop leaves the room
                        };
                        let mut events = client.events();
                        let mut lifecycle_events = client.events();
                        {
                            // Store + drain under ONE client-lock critical
                            // section: the subscription pushes to the buffer
                            // while holding this same lock, so every commit
                            // is either drained here or enqueued directly
                            // after — never dropped between (verify pass).
                            let mut client_slot = lock(&handle.chat2);
                            if host.inner.edge_disconnected.load(Ordering::Acquire) { return; }
                            let pending: Vec<(String, Vec<u8>)> =
                                std::mem::take(&mut *lock(&handle.chat2_pending_local));
                            for (batch_id, update) in pending {
                                client.enqueue_batch(batch_id, update);
                            }
                            *client_slot = Some(client);
                        }
                        tracing::info!(chat = %chat, "chat2 room joined (converged)");
                        // A missed event, failed POST or actor restart must not
                        // forget rejected operations. Any author can checkpoint
                        // its own durable history, including a non-host desktop.
                        let checkpoint_host = host.clone();
                        let checkpoint_weak = weak.clone();
                        host.spawn_worker(async move {
                            loop {
                                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                                let Some(handle) = checkpoint_weak.upgrade() else { return };
                                if checkpoint_host.inner.edge_disconnected.load(Ordering::Acquire) { return; }
                                let known = lock(&handle.chat2).as_ref().is_some_and(|c|c.stats().server_known);
                                if known && checkpoint_host.inner.store.rejected_chat_updates(&handle.chat_id).is_ok_and(|v| !v.is_empty()) {
                                    checkpoint_host.spawn_chat2_checkpoint(&handle, "durable-rejection");
                                }
                            }
                        });

                        // Bootstrap heal: a room with NO checkpoint can't
                        // cover its rows' causal deps for cold readers — a
                        // pre-0.1.34 first contact whose init batch never
                        // went up (every reader parks every row on missing
                        // deps, transcript invisible forever), or a host
                        // whose WS pushes strand. The checkpoint is the
                        // universal patch: full doc over plain HTTP. Checked
                        // once, shortly after join (an idle chat never hits
                        // the quiesce tick, so the tick can't be the only
                        // trigger).
                        if host.is_host(&chat) {
                            let host = host.clone();
                            let weak = weak.clone();
                            host.clone().spawn_worker(async move {
                                // With the pull-first transport the client
                                // constructs before any state answer — wait
                                // until the server's view is KNOWN (bounded)
                                // or the all-zero placeholder stats would
                                // misread as "no checkpoint" and upload a
                                // spurious full-doc heal on a thin link.
                                for _ in 0..40u32 {
                                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                                    let Some(handle) = weak.upgrade() else { return };
                                    let known = lock(&handle.chat2)
                                        .as_ref()
                                        .is_some_and(|c| c.stats().server_known);
                                    if known {
                                        break;
                                    }
                                }
                                let Some(handle) = weak.upgrade() else { return };
                                let no_checkpoint = lock(&handle.chat2)
                                    .as_ref()
                                    .is_some_and(|c| c.stats().server_known && c.stats().checkpoint_size == 0);
                                let has_content = handle
                                    .doc
                                    .read_entries()
                                    .map(|e| !e.is_empty())
                                    .unwrap_or(false);
                                if no_checkpoint && has_content {
                                    tracing::info!(chat = %handle.chat_id,
                                        "chat2 room has rows but no checkpoint; posting bootstrap checkpoint");
                                    host.spawn_chat2_checkpoint(&handle, "bootstrap");
                                }
                            });
                        }
                        // Host recovery duties (C3): a wiped room needs a
                        // seed checkpoint or fresh readers see only
                        // post-reset rows; rejected pushes reach peers only
                        // through a checkpoint. Watcher dies with the handle.
                        if host.is_host(&chat) {
                            let host = host.clone();
                            let weak = weak.clone();
                            let chat = chat.clone();
                            host.clone().spawn_worker(async move {
                                use luna_sync::chat_client::ChatEvent;
                                loop {
                                    match events.recv().await {
                                        Ok(ChatEvent::ServerReset) => {
                                            let Some(handle) = weak.upgrade() else { return };
                                            tracing::warn!(chat = %chat, "chat2 room reset; posting seed checkpoint");
                                            host.spawn_chat2_checkpoint(&handle, "server-reset");
                                        }
                                        Ok(ChatEvent::PushRejected) => {
                                            let Some(handle) = weak.upgrade() else { return };
                                            tracing::warn!(chat = %chat, "chat2 push rejected; compensating via checkpoint");
                                            host.spawn_chat2_checkpoint(&handle, "push-rejected");
                                        }
                                        Ok(_) => {}
                                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                                    }
                                }
                            });
                        }
                        drop(handle);
                        if token_changes.is_none() {
                            return;
                        }
                        loop {
                            tokio::select! {
                                event = lifecycle_events.recv() => match event {
                                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                                },
                                _ = crate::workspace_host::token_changed(&mut token_changes) => {
                                    if matches!(edge.bearer().await, Err(luna_rpc::TokenError::SignedOut)) {
                                        if let Some(handle) = weak.upgrade() {
                                            lock(&handle.chat2).take();
                                            // Keep journaling local cleanup after credentials disappear.
                                        }
                                        tracing::info!(chat = %chat,
                                            "chat2 credentials removed; leaving room");
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(chat = %chat, error = %err,
                            backoff_ms = backoff.as_millis() as u64,
                            "chat2 join failed; retrying");
                    }
                    Err(_) => {
                        tracing::warn!(chat = %chat,
                            backoff_ms = backoff.as_millis() as u64,
                            "chat2 join timed out; retrying");
                    }
                }
                // Drain stale online events first: only successes DURING this
                // wait count, or our own last dial would cut every wait to
                // zero (same discipline as chat_client's wait_backoff).
                while online.try_recv().is_ok() {}
                tokio::select! {
                    _ = tokio::time::sleep(backoff + crate::workspace_host::join_retry_jitter()) => {
                        backoff = (backoff * 2).min(crate::workspace_host::JOIN_RETRY_CAP);
                    }
                    _ = wake.recv() => {
                        backoff = crate::workspace_host::JOIN_RETRY_BASE;
                    }
                    _ = online.recv() => {
                        backoff = crate::workspace_host::JOIN_RETRY_BASE;
                    }
                    _ = crate::workspace_host::token_changed(&mut token_changes) => {
                        backoff = crate::workspace_host::JOIN_RETRY_BASE;
                    }
                }
            }
        });
    }

    /// Host-side chat2 seed (docs/chat2-sync.md M1/M2): rebuild thin, POST
    /// the seed checkpoint, persist the thin lineage locally, THEN flip the
    /// registry — each step idempotent, a crash before the flip leaves the
    /// chat on s2 and the next open retries (M5).
    /// Defer a chat2 seed until the chat is verifiably quiet: the s2 room has
    /// joined (the rebuild must include every row the room holds — seeding
    /// from the pre-backfill local doc forked other devices' rows into the
    /// retired lineage) and the doc frontier has stopped moving (a
    /// nudge-burst in flight — an incoming queued command — must land and
    /// execute first). Holds only a weak handle: eviction ends the wait.
    fn spawn_chat2_seed_when_quiet(
        &self,
        edge: EdgeConfig,
        chat_id: &str,
        handle: &Arc<ChatDocHandle>,
    ) {
        if self.is_side_document(chat_id) {
            return;
        }
        const TICK: std::time::Duration = std::time::Duration::from_millis(500);
        const QUIET_TICKS: u32 = 4; // 2s of frontier silence
        if !lock(&self.inner.seed_waiting).insert(chat_id.to_string()) {
            return; // a quiet-waiter is already armed for this chat
        }
        let host = self.clone();
        let chat = chat_id.to_string();
        let weak = Arc::downgrade(handle);
        self.spawn_worker(async move {
            async {
                // The old join-wait gate (seed only after the s2 room had
                // backfilled) is gone with the s2 client: no device writes
                // an s2 room anymore, so the host's local doc IS the
                // authority for a straggler gen-1 chat. Only the quiet gate
                // remains (no seed under a moving frontier).
                let mut quiet = 0u32;
                let mut last_vv: Option<Vec<u8>> = None;
                loop {
                    tokio::time::sleep(TICK).await;
                    let Some(handle) = weak.upgrade() else { return };
                    if handle.retired.load(Ordering::Relaxed) {
                        return;
                    }
                    let vv = handle.doc.doc().oplog_vv().encode();
                    if last_vv.as_ref() == Some(&vv) {
                        quiet += 1;
                        if quiet >= QUIET_TICKS {
                            let doc = handle.doc.clone();
                            drop(handle);
                            host.spawn_chat2_seed(edge, &chat, doc);
                            return;
                        }
                    } else {
                        quiet = 0;
                        last_vv = Some(vv);
                    }
                }
            }
            .await;
            // Cleared on EVERY exit path so an aborted wait (evicted handle,
            // room never joined, seed handed off) can re-arm later.
            lock(&host.inner.seed_waiting).remove(&chat);
        });
    }

    fn spawn_chat2_seed(&self, edge: EdgeConfig, chat_id: &str, doc: Arc<SessionDoc>) {
        {
            let mut seeding = lock(&self.inner.seeding);
            if !seeding.insert(chat_id.to_string()) {
                return; // seed already in flight
            }
        }
        let host = self.clone();
        let chat = chat_id.to_string();
        self.spawn_worker(async move {
            let outcome = host.seed_chat2(&edge, &chat, doc).await;
            lock(&host.inner.seeding).remove(&chat);
            match outcome {
                Ok(()) => {
                    tracing::info!(chat = %chat, "chat2 seeded; registry flipped to roomGen 2");
                }
                Err(err) => {
                    tracing::warn!(chat = %chat, error = %err,
                        "chat2 seed failed; chat stays on s2 (retries next open)");
                }
            }
        });
    }

    async fn seed_chat2(
        &self,
        edge: &EdgeConfig,
        chat_id: &str,
        doc: Arc<SessionDoc>,
    ) -> Result<(), String> {
        if self.is_side_document(chat_id) {
            return Err("Side chat documents remain in their owning profile".into());
        }
        use base64::Engine as _;
        let vv_at_rebuild = doc.doc().oplog_vv().encode();
        let rebuilt = luna_doc::rebuild::rebuild_thin_doc(&doc).map_err(|e| e.to_string())?;
        // The seed's own doc ref must be gone before the pinned re-check
        // below: `pinned` reads `Arc::strong_count(&handle.doc) > 1`, and
        // holding this clone made that true unconditionally — every seed
        // aborted "became active mid-seed" and the cutover never flipped a
        // single chat (v0.1.32 DOA).
        drop(doc);
        if !rebuilt.sidecar.is_empty() {
            // Sidecar PARKED (docs/chat2-sync.md A2): full outputs are not
            // uploaded; they survive in the rollback snapshot + run journal.
            tracing::info!(chat = %chat_id, payloads = rebuilt.sidecar.len(),
                "chat2 seed: sidecar parked; outputs stay local");
        }
        let snapshot = rebuilt.doc.export_snapshot().map_err(|e| e.to_string())?;
        let frontier = rebuilt.doc.doc().oplog_vv().encode();
        let bearer = edge.bearer().await.map_err(|e| e.to_string())?;
        let url = format!(
            "{}/chat2/{}/checkpoint?seqCovered=0",
            edge.url.trim_end_matches('/'),
            chat_id
        );
        let res = self
            .inner
            .http
            .post(&url)
            .bearer_auth(&bearer)
            .header(
                "x-chat2-frontier",
                base64::engine::general_purpose::STANDARD.encode(&frontier),
            )
            .body(snapshot.clone())
            .send()
            .await
            .map_err(|e| format!("seed checkpoint POST: {}", describe_http_error(e)))?;
        if !res.status().is_success() {
            return Err(format!("seed checkpoint HTTP {}", res.status()));
        }
        // PINNED RE-CHECK before anything irreversible (review B1): if a
        // run/watcher attached during the rebuild+POST, abort — everything
        // they write would fork away from the thin lineage. The orphan
        // checkpoint in the chat2 room is harmless (wholly replaced by the
        // next seed's seqCovered=0 POST); the chat stays on s2 and the next
        // quiet open retries.
        {
            let handles = lock(&self.inner.handles);
            if let Some(handle) = handles.get(chat_id) {
                if self.seed_blocked(handle) {
                    return Err("chat became active mid-seed; aborted before flip".into());
                }
                // Frontier seal (review B1's TOCTOU): ANY doc movement since
                // the rebuild — a synced remote row, a local write that has
                // already released its doc ref — means the thin lineage is
                // missing it. Borrowed read (no Arc clone: that would trip
                // the pinned check we just passed).
                if handle.doc.doc().oplog_vv().encode() != vv_at_rebuild {
                    return Err("doc advanced mid-seed; aborted before flip".into());
                }
            }
        }
        // Rollback copy of the fat lineage BEFORE the thin one replaces it —
        // never overwriting an existing copy (review B5).
        let rollback_id = format!("{chat_id}.pre-chat2");
        if matches!(self.inner.store.load_snapshot(&rollback_id), Ok(None))
            && let Ok(Some(old)) = self.inner.store.load_snapshot(chat_id)
        {
            let _ = self.inner.store.save_snapshot(&rollback_id, &old);
        }
        self.inner
            .store
            .save_snapshot_with_cursor(chat_id, &snapshot, 0, crate::chat2_host::CHAT2_DOC_EPOCH)
            .map_err(|e| e.to_string())?;
        // Registry flip LAST — the cutover signal every device dials by.
        let flipped = self
            .workspace()
            .ok_or("no workspace host")?
            .set_chat_room_gen(chat_id, 2)
            .map_err(|e| e.to_string())?;
        if !flipped {
            return Err("chat row vanished during seed".into());
        }
        // The live handle still holds the FAT doc on the s2 room. Retire it:
        // it must never persist again (it would clobber the thin lineage);
        // drop it entirely when unpinned so the next open converges onto
        // chat2. A pinned (watched/running) handle keeps working against s2
        // until it closes — the flip is registry-side, readers already moved.
        let dropped = {
            let mut handles = lock(&self.inner.handles);
            if let Some(handle) = handles.get(chat_id) {
                handle.retired.store(true, Ordering::Relaxed);
                // Drop unless a live WRITER holds the doc. Watchers do not
                // keep the fat handle alive: their streams end with it and
                // they resubscribe onto the chat2 adopt path (the same
                // contract the cutover watcher enforces for remote flips).
                if Arc::strong_count(&handle.doc) == 1 {
                    handles.remove(chat_id);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        tracing::info!(chat = %chat_id, handle_dropped = dropped, "chat2 seed complete");
        Ok(())
    }

    /// Boot-time transcript salvage (born-gen2 aftermath, 2026-08-11): a chat
    /// we host whose chat2 doc has NO message entries while its run journal
    /// has events lost its transcript to a stuck s2 handle (the retired flag
    /// suppressed every snapshot save; the post-restart reopen born a blank
    /// lineage). The full fat doc still exists in the legacy s2 room — the
    /// stuck engine pushed every op into it until it died — and sometimes in
    /// a `.pre-chat2` rollback on disk. Re-append its entries (thinned) into
    /// the LIVE chat2 lineage as ordinary incremental updates: no lineage
    /// replacement, no checkpoint surgery, every device converges through
    /// the normal room flow. Idempotent: a doc with any message entry is
    /// never touched, and the salvage only runs on the hosting device.
    pub fn spawn_transcript_salvage(&self, journals_dir: std::path::PathBuf) {
        let host = self.clone();
        self.spawn_worker(async move {
            // Let boot settle (registry load, room joins) before sweeping.
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let Some(ws) = host.workspace() else { return };
            let chats: Vec<luna_proto::Chat> = ws.watch_chats().borrow().clone();
            for chat in chats {
                if host.inner.store.is_side_chat(&chat.id).unwrap_or(true) {
                    continue;
                }
                if chat.device_id != host.inner.config.device_id {
                    continue; // only the host owns its chats' history
                }
                if chat.room_gen.unwrap_or(1) < 2 {
                    continue; // still s2-mode: transcript lives in its room
                }
                let journal = journals_dir.join(format!("{}.jsonl", chat.id));
                let journaled = std::fs::metadata(&journal)
                    .map(|m| m.len() > 0)
                    .unwrap_or(false);
                if !journaled {
                    continue; // never ran here — an empty doc is just new
                }
                if let Err(err) = host.salvage_chat_transcript(&chat.id).await {
                    tracing::warn!(chat = %chat.id, error = %err, "transcript salvage failed");
                }
            }
        });
    }

    async fn salvage_chat_transcript(&self, chat_id: &str) -> Result<(), String> {
        let handle = self.open(chat_id).map_err(|e| e.to_string())?;
        if !handle
            .doc()
            .read_entries()
            .map_err(|e| e.to_string())?
            .is_empty()
        {
            return Ok(()); // transcript present — nothing lost
        }
        // Fat source: the M3 adopt's rollback copy on disk. (The other
        // source — the legacy s2 room — went away with the s2 client; any
        // transcript that existed only there was salvaged by earlier
        // releases or is reachable in the room's storage server-side.)
        let rollback_id = format!("{chat_id}.pre-chat2");
        let fat_bytes = self.inner.store.load_snapshot(&rollback_id).ok().flatten();
        let Some(bytes) = fat_bytes else {
            return Ok(()); // no fat lineage anywhere — genuinely empty chat
        };
        let raw = loro::LoroDoc::new();
        raw.import(&bytes).map_err(|e| e.to_string())?;
        let fat = SessionDoc::from_doc(raw);
        // Thin before appending (docs/chat2-sync.md A2): full outputs are
        // parked, exactly like a seed — they survive in the rollback copy
        // saved below and the run journal.
        let rebuilt = luna_doc::rebuild::rebuild_thin_doc(&fat).map_err(|e| e.to_string())?;
        let entries = rebuilt.doc.read_entries().map_err(|e| e.to_string())?;
        if entries.is_empty() {
            return Ok(());
        }
        if matches!(self.inner.store.load_snapshot(&rollback_id), Ok(None)) {
            let _ = self.inner.store.save_snapshot(&rollback_id, &bytes);
        }
        // Re-check emptiness at the last instant: a run that started during
        // the room fetch must not get history interleaved under it.
        if !handle
            .doc()
            .read_entries()
            .map_err(|e| e.to_string())?
            .is_empty()
        {
            return Err("doc gained entries mid-salvage; aborted".into());
        }
        for entry in &entries {
            handle
                .doc()
                .push_message(entry)
                .map_err(|e| e.to_string())?;
        }
        tracing::info!(chat = %chat_id, entries = entries.len(),
            "transcript salvaged into chat2 lineage");
        Ok(())
    }

    /// chat2 host duties on the doc-quiesce tick (docs/chat2-sync.md C3):
    /// - threshold checkpoint: when the room's row log passes 512KB or 200
    ///   rows, post a full checkpoint so cold readers load one compact blob
    ///   instead of replaying the log (the alert-shaped growth bound);
    /// - tail sidecar: publish the last-64 transcript JSON for thin/instant
    ///   readers (the iOS fallback path).
    async fn chat2_maintenance(&self, handle: &Arc<ChatDocHandle>) {
        if self.is_side_document(&handle.chat_id) || handle.retired.load(Ordering::Relaxed) {
            return;
        }
        let stats = match &*lock(&handle.chat2) {
            Some(client) => client.stats(),
            None => return,
        };
        let Some(edge) = self.inner.config.edge.clone() else {
            return;
        };
        let chat_id = handle.chat_id.clone();
        // Tail publish: cheap, every quiesce tick.
        if let Ok(tail) =
            luna_doc::materialize_tail(&handle.doc, now_ms(), luna_doc::TAIL_MESSAGE_COUNT)
            && let Ok(body) = serde_json::to_vec(&tail)
        {
            let http = self.inner.http.clone();
            let edge_tail = edge.clone();
            let chat = chat_id.clone();
            self.spawn_worker(async move {
                let Ok(bearer) = edge_tail.bearer().await else {
                    return;
                };
                let url = format!(
                    "{}/chat2/{}/tail",
                    edge_tail.url.trim_end_matches('/'),
                    chat
                );
                let _ = http
                    .put(&url)
                    .bearer_auth(&bearer)
                    .header("content-type", "application/json")
                    .body(body)
                    .send()
                    .await;
            });
        }
        // Threshold checkpoint (rowBytes > 512KB || rows > 200), one in
        // flight at a time (review H1).
        if stats.row_bytes <= 512 * 1024 && stats.row_count <= 200 {
            return;
        }
        self.spawn_chat2_checkpoint(handle, "threshold");
    }

    /// POST a full checkpoint for a chat2 room (one in flight per handle).
    /// Callers: the quiesce-tick threshold above, and the client recovery
    /// events (`ServerReset` — a wiped room needs a seed checkpoint or every
    /// fresh reader sees only post-reset rows; `PushRejected` — the rejected
    /// ops reach peers only through a checkpoint).
    fn spawn_chat2_checkpoint(&self, handle: &Arc<ChatDocHandle>, reason: &'static str) {
        if self.is_side_document(&handle.chat_id)
            || self.inner.edge_disconnected.load(Ordering::Acquire)
        {
            return;
        }
        use base64::Engine as _;
        let Some(edge) = self.inner.config.edge.clone() else {
            return;
        };
        let stats = match &*lock(&handle.chat2) {
            Some(client) => client.stats(),
            None => return,
        };
        let chat_id = handle.chat_id.clone();
        if handle
            .checkpointing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let in_flight = handle.checkpointing.clone();
        let rejected = self
            .inner
            .store
            .rejected_chat_updates(&chat_id)
            .unwrap_or_default();
        let publication_store = self.inner.store.clone();
        let Ok(snapshot) = handle.doc.export_snapshot() else {
            in_flight.store(false, Ordering::Release);
            return;
        };
        let frontier = match loro::LoroDoc::decode_import_blob_meta(&snapshot, true) {
            Ok(meta) => meta.partial_end_vv.encode(),
            Err(err) => {
                tracing::error!(%err, "chat2: checkpoint metadata decode failed");
                in_flight.store(false, Ordering::Release);
                return;
            }
        };
        let snapshot_vv = loro::VersionVector::decode(&frontier).expect("encoded snapshot vector");
        let covered_rejections: Vec<String> = rejected
            .into_iter()
            .filter_map(|(id, bytes)| {
                loro::LoroDoc::decode_import_blob_meta(&bytes, true)
                    .ok()
                    .filter(|m| snapshot_vv.includes_vv(&m.partial_end_vv))
                    .map(|_| id)
            })
            .collect();
        let seq_covered = stats.cursor;
        let http = self.inner.http.clone();
        let weak_note = Arc::downgrade(handle);
        self.spawn_worker(async move {
            let Ok(bearer) = edge.bearer().await else {
                in_flight.store(false, Ordering::Release);
                return;
            };
            let url = format!(
                "{}/chat2/{}/checkpoint?seqCovered={}",
                edge.url.trim_end_matches('/'),
                chat_id,
                seq_covered
            );
            let size = snapshot.len() as u64;
            match http
                .post(&url)
                .bearer_auth(&bearer)
                .header(
                    "x-chat2-frontier",
                    base64::engine::general_purpose::STANDARD.encode(&frontier),
                )
                .body(snapshot)
                .send()
                .await
            {
                Ok(res) if res.status().is_success() => {
                    tracing::info!(chat = %chat_id, seq_covered, reason, "chat2 checkpoint posted");
                    for batch_id in &covered_rejections {
                        if let Err(err) = publication_store.acknowledge_chat_update(&chat_id,batch_id) {
                            tracing::warn!(%err, "chat2: checkpoint obligation retirement failed; will retry");
                        }
                    }

                    if let Some(handle) = weak_note.upgrade()
                        && let Some(client) = &*lock(&handle.chat2)
                    {
                        client.note_checkpoint(seq_covered, size);
                    }
                }
                Ok(res) => {
                    tracing::warn!(chat = %chat_id, status = res.status().as_u16(),
                        "chat2 checkpoint rejected");
                }
                Err(err) => {
                    let err = describe_http_error(err);
                    tracing::warn!(chat = %chat_id, error = %err, "chat2 checkpoint POST failed");
                }
            }
            in_flight.store(false, Ordering::Release);
        });
    }

    /// LRU eviction: while the warm set exceeds [`WARM_DOC_CAP`] or the
    /// resident estimate exceeds `DOC_LRU_BYTE_BUDGET`, close the
    /// least-recently-touched unpinned docs. Pinned (never evicted):
    /// - watched docs (`messages_tx` has receivers — a UI transcript);
    /// - docs with a live writer (`Arc<SessionDoc>` held outside the handle —
    ///   a run streaming into it);
    /// - host-side docs with pending commands (the executor owes them work).
    ///
    /// Eviction flushes a final snapshot, so reopen loses nothing; missed
    /// remote updates re-arrive through the room join's VV backfill.
    fn evict_over_budget(&self) {
        let mut by_age: Vec<(i64, String)> = {
            let handles = lock(&self.inner.handles);
            handles
                .values()
                .map(|h| (h.last_access.load(Ordering::Relaxed), h.chat_id.clone()))
                .collect()
        };
        by_age.sort_unstable();
        for (last_access, chat_id) in by_age {
            if now_ms() - last_access < EVICT_MIN_IDLE_MS {
                // Sorted oldest-first: everything after this is younger.
                return;
            }
            let (count, estimate) = {
                let handles = lock(&self.inner.handles);
                (
                    handles.len(),
                    handles
                        .values()
                        .map(|h| h.resident_estimate())
                        .sum::<usize>(),
                )
            };
            if count <= WARM_DOC_CAP && estimate <= luna_doc::DOC_LRU_BYTE_BUDGET {
                return;
            }
            let evicted = {
                let mut handles = lock(&self.inner.handles);
                match handles.get(&chat_id) {
                    Some(handle) if !self.pinned(handle) => handles.remove(&chat_id),
                    _ => None,
                }
            };
            if let Some(handle) = evicted {
                // Final flush outside the map lock; ≤1s of changes could be
                // pending in the snapshot debounce.
                self.save_snapshot(&handle);
                tracing::debug!(chat = %handle.chat_id, "doc evicted (LRU)");
            }
        }
    }

    /// Seed-specific activity gate: a live WRITER (a run's doc ref) or
    /// pending host commands block a seed — watchers do NOT. Pure readers
    /// cannot lose writes, and the flip drops the handle so their streams
    /// end and resubscribe onto the chat2 adopt path. `pinned()` below keeps
    /// counting watchers for EVICTION, where a watched doc must stay
    /// resident. (Watcher-pinned seeds made "open a chat to look at it"
    /// self-defeating: the act of viewing blocked its own migration.)
    fn seed_blocked(&self, handle: &Arc<ChatDocHandle>) -> bool {
        if Arc::strong_count(&handle.doc) > 1 {
            return true;
        }
        if self.is_host(&handle.chat_id) {
            let is_processed = |id: &str| self.inner.store.is_processed(id).unwrap_or(false);
            match handle.doc.read_commands() {
                Ok(commands) => commands
                    .iter()
                    .any(|c| c.status == SessionCommandStatus::Pending && !is_processed(&c.id)),
                // Unreadable ledger: never flip blind.
                Err(_) => true,
            }
        } else {
            false
        }
    }

    fn pinned(&self, handle: &Arc<ChatDocHandle>) -> bool {
        // Durable batches may outlive this handle. Only failed disk writes
        // require retaining the in-memory copy until persistence recovers.
        if handle.publication_failed.load(Ordering::Acquire) {
            return true;
        }
        if handle.messages_tx.receiver_count() > 0 {
            return true;
        }
        // The handle itself holds one doc ref; more means a live writer.
        if Arc::strong_count(&handle.doc) > 1 {
            return true;
        }
        if self.is_host(&handle.chat_id) {
            let is_processed = |id: &str| self.inner.store.is_processed(id).unwrap_or(false);
            match handle.doc.read_commands() {
                Ok(commands) => commands
                    .iter()
                    .any(|c| c.status == SessionCommandStatus::Pending && !is_processed(&c.id)),
                // Unreadable ledger: keep the doc, never evict blind.
                Err(_) => true,
            }
        } else {
            false
        }
    }

    /// Probe every open chat's room (window-focus liveness sweep). Each
    /// room ignores the hint unless it has been broadcast-quiet ≥30s.
    pub fn probe_open_chats(&self) {
        let handles: Vec<Arc<ChatDocHandle>> =
            lock(&self.inner.handles).values().cloned().collect();
        for handle in handles {
            // chat2 rooms verify liveness on user signals — a
            // deaf-but-ponging DO otherwise freezes a watched transcript
            // for the whole background probe quiet window.
            if let Some(chat2) = lock(&handle.chat2).as_ref() {
                chat2.probe();
            }
        }
    }

    /// Window-focus fast path: one cheap HTTP probe of the edge decides
    /// whether to un-park every reconnect backoff NOW (success → online
    /// event → immediate redials with fresh backoff) or to leave them
    /// backing off (failure — a dial can't succeed either, so don't burn
    /// the attempt). Recovery rides the "user looked at the app" event
    /// instead of timer luck.
    pub fn probe_edge_reachability(&self) {
        let Some(edge) = self.inner.config.edge.clone() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let url = format!("{}/health", edge.url.trim_end_matches('/'));
        let http = self.inner.http.clone();
        self.spawn_worker_on(&runtime, async move {
            let res = http
                .get(&url)
                .timeout(std::time::Duration::from_secs(3))
                .send()
                .await;
            if let Ok(res) = res
                && res.status().is_success()
            {
                luna_sync::wake::notify_online();
            }
        });
    }

    /// The in-flight queued-attachment transfer set: current entries first,
    /// then a fresh snapshot per landed chunk (see `push_attachments`).
    pub fn watch_transfers(&self) -> watch::Receiver<Vec<luna_proto::TransferProgress>> {
        self.inner.transfers.subscribe()
    }

    /// Publish one transfer's progress (upserted by uploadId).
    fn transfer_progress_set(&self, upload_id: &str, file_name: &str, done: u64, total: u64) {
        self.inner.transfers.send_modify(|list| {
            match list.iter_mut().find(|t| t.upload_id == upload_id) {
                Some(t) => {
                    t.done = done;
                    t.total = total;
                }
                None => list.push(luna_proto::TransferProgress {
                    upload_id: upload_id.to_string(),
                    file_name: file_name.to_string(),
                    done,
                    total,
                }),
            }
        });
    }

    /// Retire a transfer's progress entry (commit landed, or the attempt
    /// failed and the retry will re-publish).
    fn transfer_progress_clear(&self, upload_id: &str) {
        self.inner.transfers.send_modify(|list| {
            list.retain(|t| t.upload_id != upload_id);
        });
    }

    /// The connectivity stream: current posture first, then every change.
    /// Lazily starts a monitor — a 1s recompute over in-memory stats
    /// (atomics + small locks), published only when the value changes. The
    /// retry countdown renders client-side from `retry_at_ms`, so quiet
    /// periods emit nothing at all.
    pub fn watch_connectivity(&self) -> watch::Receiver<luna_proto::Connectivity> {
        let tx = self
            .inner
            .connectivity
            .get_or_init(|| watch::channel(self.compute_connectivity()).0);
        let rx = tx.subscribe();
        if !self.inner.connectivity_started.swap(true, Ordering::SeqCst)
            && tokio::runtime::Handle::try_current().is_ok()
        {
            let host = self.clone();
            self.spawn_worker(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    let next = host.compute_connectivity();
                    if let Some(tx) = host.inner.connectivity.get() {
                        tx.send_if_modified(|cur| {
                            if *cur == next {
                                false
                            } else {
                                *cur = next;
                                true
                            }
                        });
                    }
                }
            });
        }
        rx
    }

    /// One snapshot of the edge posture: OS path status beats registry-room
    /// state beats per-chat rooms. `Disabled` (the default) = local profile.
    ///
    /// Degradation is HYSTERETIC (v0.2.12 feedback): a room mid-join, an
    /// idle link waking for a send, or a navigation-triggered dial all read
    /// "disconnected" for a few hundred ms on a healthy network — surfacing
    /// those flashed amber warnings and "Queued" badges at every chat
    /// switch. Raw degradation must persist [`DEGRADE_GRACE`] before it is
    /// reported; recovery reports instantly.
    fn compute_connectivity(&self) -> luna_proto::Connectivity {
        use luna_proto::{ChatConnectivity, Connectivity, ConnectivityState};
        let workspace = self.workspace();
        let edge_expected = self.inner.config.edge.is_some()
            || workspace.as_ref().is_some_and(|w| w.edge_expected());
        if !edge_expected {
            return Connectivity::default();
        }
        let now = std::time::Instant::now();
        let mut grace = lock(&self.inner.connectivity_grace);
        let statuses = self.sync_statuses();
        grace.retain_chats(|id| statuses.iter().any(|(chat_id, _)| chat_id == id));
        let chats = statuses
            .into_iter()
            .filter(|(chat_id, _)| !self.is_side_document(chat_id))
            .map(|(chat_id, stats)| {
                let stats = stats.unwrap_or_default();
                let connected = !grace.degraded(GraceKey::Chat(&chat_id), !stats.connected, now);
                ChatConnectivity {
                    chat_id,
                    connected,
                    pending_pushes: stats.pending_pushes,
                }
            })
            .collect();
        let reconnect = workspace.as_ref().and_then(|w| w.reconnect_state());
        let registry_connected = workspace
            .as_ref()
            .and_then(|w| w.sync_status())
            .is_some_and(|s| s.connected);
        let path_offline =
            grace.degraded(GraceKey::OsPath, luna_sync::wake::path_is_offline(), now);
        let registry_down = grace.degraded(GraceKey::Registry, !registry_connected, now);
        let (state, retry_at_ms, last_failure) = if path_offline {
            (
                ConnectivityState::Offline,
                0,
                reconnect.and_then(|r| r.last_failure),
            )
        } else if !registry_down {
            (ConnectivityState::Connected, 0, None)
        } else {
            let reconnect = reconnect.unwrap_or_default();
            (
                ConnectivityState::Reconnecting,
                reconnect.retry_at_ms,
                reconnect.last_failure,
            )
        };
        Connectivity {
            state,
            retry_at_ms,
            last_failure,
            chats,
        }
    }

    /// Per-open-chat room introspection for SyncStatus / `luna sync`.
    /// `None` room = still dialing (join retry loop) or edge-less.
    pub fn sync_statuses(&self) -> Vec<(String, Option<luna_sync::ChatStatsSnapshot>)> {
        let handles: Vec<Arc<ChatDocHandle>> =
            lock(&self.inner.handles).values().cloned().collect();
        let mut rows: Vec<(String, Option<luna_sync::ChatStatsSnapshot>)> = handles
            .iter()
            .map(|h| {
                (
                    h.chat_id.clone(),
                    lock(&h.chat2).as_ref().map(|client| client.stats()),
                )
            })
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }

    /// Drop a chat's doc unconditionally and delete its local snapshot — the
    /// chat is gone (DeleteChat / DeleteSpace cascade). Watchers see the
    /// stream end; a racing writer keeps its orphaned doc until the run ends.
    pub fn purge_chat(&self, chat_id: &str) {
        self.evict_chat(chat_id);
        if let Err(err) = self.inner.store.delete_snapshot(chat_id) {
            tracing::warn!(chat = %chat_id, error = %err, "snapshot delete failed");
        }
    }

    /// Finish an atomic task-command delete after its local snapshot/outbox
    /// rows were removed in the receipt transaction.
    pub(crate) fn flush_chat_checked(&self, chat_id: &str) -> Result<(), EngineError> {
        let handle = self.open(chat_id)?;
        self.save_snapshot_checked(&handle)
    }

    pub(crate) fn evict_chat(&self, chat_id: &str) {
        let removed = lock(&self.inner.handles).remove(chat_id);
        drop(removed);
    }

    /// Composer path: append an immutable pending command entry (rule 1). Durable by
    /// construction — the change subscription kicks the drain, so a local host executes
    /// immediately and an offline doc simply holds the entry until it syncs.
    pub fn queue_command(
        &self,
        chat_id: &str,
        payload: SessionCommandPayload,
    ) -> Result<String, EngineError> {
        self.queue_command_with_transfers(chat_id, payload, Vec::new())
    }

    /// [`Self::queue_command`] plus queued-attachment transfers: the command's
    /// `pending://` refs name bytes already committed to THIS device's uploads
    /// dir; when another device hosts the chat, a background task pushes them
    /// over the peer link (retry until they land) while the command is
    /// already durably queued. The send is a local write — attachment bytes
    /// chase it, never gate it (2026-08-19 incident).
    pub fn queue_command_with_transfers(
        &self,
        chat_id: &str,
        payload: SessionCommandPayload,
        transfers: Vec<crate::uploads::AttachmentTransfer>,
    ) -> Result<String, EngineError> {
        let id = new_id();
        self.queue_command_with_id_and_transfers(chat_id, &id, payload, transfers)?;
        Ok(id)
    }

    /// L07 authoritative enqueue. The caller-minted id is both the durable
    /// command receipt key and the existing session-doc command id.
    pub fn queue_command_with_id_and_transfers(
        &self,
        chat_id: &str,
        command_id: &str,
        payload: SessionCommandPayload,
        transfers: Vec<crate::uploads::AttachmentTransfer>,
    ) -> Result<(), EngineError> {
        let admission_w = self
            .inner
            .store
            .command_workspace_repair_revision(command_id)?
            .unwrap_or(self.inner.store.workspace_repair_revision(chat_id)?);
        let handle = self.open(chat_id)?;
        let now = now_ms();
        let based_on = handle.doc.read_entries()?.last().map(|m| CommandBasedOn {
            turn_id: Some(m.id.clone()),
            frontier: None,
        });
        let is_message = matches!(
            payload,
            SessionCommandPayload::Run { .. } | SessionCommandPayload::Steer { .. }
        );
        let entry = SessionCommandEntry {
            id: command_id.to_string(),
            payload,
            issued_by: self.inner.config.device_id.clone(),
            issued_at: now,
            based_on,
            expires_at: Some(now + COMMAND_DEFAULT_TTL_MS),
            status: SessionCommandStatus::Pending,
            resolution: None,
        };
        if !self
            .inner
            .store
            .record_local_executable_at_workspace_revision(
                chat_id,
                LocalExecutableKind::Command,
                command_id,
                &executable_row_hash(&entry)?,
                None,
                admission_w,
            )?
        {
            return Err(EngineError::Other(
                "command id already has different local authority".into(),
            ));
        }
        handle.doc.queue_command(&entry)?;
        // Sending a message revives an archived chat: the user is acting in it
        // again, so the LWW row flips back to active on every device. Best-
        // effort — the command itself is durable regardless.
        if is_message {
            self.unarchive_on_send(chat_id);
        }
        // §7 durable delivery: when another device hosts this chat, nudge its device
        // room so a cold host opens the doc and drains the queue. Fire-and-forget —
        // the command is durable in the doc either way (a host that opens the chat
        // for any other reason still executes it).
        self.nudge_remote_host(chat_id);
        self.spawn_command_delivery(chat_id, entry, transfers);
        Ok(())
    }

    /// Dormant B0a prerequisite: persist one exact Interrupt command ID as a
    /// detached snapshot/outbox projection before making it visible locally.
    /// This does not mint approval, a receipt, or source authority, and no
    /// runtime path calls it yet. A repeated ID never appends another row.
    #[allow(dead_code)]
    pub(crate) fn persist_unwired_interrupt_with_id(
        &self,
        chat_id: &str,
        command_id: &str,
    ) -> Result<bool, EngineError> {
        self.persist_unwired_interrupt_with_id_after_commit(chat_id, command_id, || Ok(()))
    }

    fn persist_unwired_interrupt_with_id_after_commit(
        &self,
        chat_id: &str,
        command_id: &str,
        after_commit: impl FnOnce() -> Result<(), EngineError>,
    ) -> Result<bool, EngineError> {
        if command_id.is_empty()
            || command_id.len() > 128
            || !command_id.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(EngineError::Other("invalid interrupt command id".into()));
        }
        let handle = self.open(chat_id)?;
        let _projection = lock(&handle.queue_projection_lock);
        if handle
            .durable_projection_uninstalled
            .load(Ordering::Acquire)
        {
            return Err(EngineError::Other(
                "a committed document projection needs recovery after restart".into(),
            ));
        }
        let prepared = self.prepare_queue_projection(&handle, command_id, |doc| {
            let existing = doc
                .read_commands()?
                .into_iter()
                .filter(|entry| entry.id == command_id)
                .collect::<Vec<_>>();
            if !existing.is_empty() {
                if existing.len() != 1
                    || !matches!(&existing[0].payload, SessionCommandPayload::Interrupt {})
                {
                    return Err(EngineError::Other(
                        "interrupt command id belongs to a different payload".into(),
                    ));
                }
                return Ok(false);
            }
            let now = now_ms();
            let based_on = doc.read_entries()?.last().map(|entry| CommandBasedOn {
                turn_id: Some(entry.id.clone()),
                frontier: None,
            });
            doc.queue_command(&SessionCommandEntry {
                id: command_id.to_string(),
                payload: SessionCommandPayload::Interrupt {},
                issued_by: self.inner.config.device_id.clone(),
                issued_at: now,
                based_on,
                expires_at: Some(now + COMMAND_DEFAULT_TTL_MS),
                status: SessionCommandStatus::Pending,
                resolution: None,
            })?;
            Ok(true)
        })?;
        if !prepared.outcome {
            return Ok(false);
        }
        let updates = prepared
            .batch_id
            .as_deref()
            .map(|batch_id| vec![(batch_id, prepared.update.as_slice())])
            .unwrap_or_default();
        // A storage error can be ambiguous. Leave this handle unable to save
        // its older live doc until a restart reloads the authoritative bytes.
        handle
            .durable_projection_uninstalled
            .store(true, Ordering::Release);
        self.inner.store.save_snapshot_with_updates_and_authority(
            &handle.chat_id,
            &prepared.snapshot,
            &updates,
            &prepared.authority_changes,
        )?;
        after_commit()?;
        let result = self.install_queue_projection(&handle, prepared);
        if result.is_ok() {
            handle
                .durable_projection_uninstalled
                .store(false, Ordering::Release);
        }
        result
    }

    /// A send revives an archived chat on every device (best-effort).
    fn unarchive_on_send(&self, chat_id: &str) {
        let Some(workspace) = self.workspace() else {
            return;
        };
        match workspace.chat(chat_id) {
            Ok(Some(chat)) if chat.archived => {
                if let Err(err) = workspace.set_chat_archived(chat_id, false) {
                    tracing::warn!(chat = %chat_id, error = %err, "unarchive on send failed");
                }
            }
            _ => {}
        }
    }

    /// Build a queue mutation on an isolated CRDT, never on the watched live
    /// document. The returned projection may be persisted atomically before
    /// its update is imported into the live handle.
    fn prepare_queue_projection<T>(
        &self,
        handle: &ChatDocHandle,
        batch_key: &str,
        mutation: impl FnOnce(&SessionDoc) -> Result<T, EngineError>,
    ) -> Result<PreparedQueueProjection<T>, EngineError> {
        let admission_w = self
            .inner
            .store
            .command_workspace_repair_revision(batch_key)?
            .unwrap_or(
                self.inner
                    .store
                    .workspace_repair_revision(&handle.chat_id)?,
            );
        let live_snapshot = handle.doc.export_snapshot()?;
        let base = if handle.retired.load(Ordering::Acquire) {
            match self
                .inner
                .store
                .load_snapshot_with_cursor(&handle.chat_id)?
            {
                Some((bytes, _, epoch)) if epoch >= crate::chat2_host::CHAT2_DOC_EPOCH => bytes,
                _ => live_snapshot,
            }
        } else {
            live_snapshot
        };
        let raw = loro::LoroDoc::new();
        raw.import(&base).map_err(|error| {
            EngineError::Other(format!("queue projection import failed: {error}"))
        })?;
        // A retired fat handle must mutate the durable thin lineage. Merge any
        // unacknowledged thin-lineage batches before deriving the candidate so
        // the full snapshot cannot discard a locally committed update.
        if handle.retired.load(Ordering::Acquire) {
            for (_, update) in self.inner.store.pending_chat_updates(&handle.chat_id)? {
                raw.import(&update).map_err(|error| {
                    EngineError::Other(format!("queue outbox replay failed: {error}"))
                })?;
            }
        }
        let candidate = SessionDoc::from_doc(raw);
        let frontier = candidate.doc().oplog_vv();
        let before_queue = candidate.read_queue()?;
        let before_commands = candidate.read_commands()?;
        let outcome = mutation(&candidate)?;
        let after_queue = candidate.read_queue()?;
        let after_commands = candidate.read_commands()?;
        let mut authority_changes = Vec::new();
        let mut unreceipted_new_commands = Vec::new();
        let prior_queue = before_queue
            .iter()
            .map(|row| (row.id.as_str(), row))
            .collect::<HashMap<_, _>>();
        let next_queue = after_queue
            .iter()
            .map(|row| (row.id.as_str(), row))
            .collect::<HashMap<_, _>>();
        for before in &before_queue {
            let after = next_queue.get(before.id.as_str()).copied();
            if after == Some(before) {
                continue;
            }
            let previous = self.inner.store.local_executable_authority(
                &handle.chat_id,
                LocalExecutableKind::Queue,
                &before.id,
            )?;
            if let Some(previous) = previous {
                let replacement = if previous.payload_hash == executable_row_hash(before)?
                    && previous.workspace_repair_revision == admission_w
                {
                    after.map(executable_row_hash).transpose()?
                } else {
                    None
                };
                authority_changes.push(LocalExecutableChange {
                    doc_id: handle.chat_id.clone(),
                    kind: LocalExecutableKind::Queue,
                    row_id: before.id.clone(),
                    expected_hash: Some(previous.payload_hash),
                    expected_workspace_repair_revision: Some(previous.workspace_repair_revision),
                    replacement_workspace_repair_revision: replacement
                        .as_ref()
                        .map(|_| previous.workspace_repair_revision),
                    replacement_hash: replacement,
                });
            }
        }
        for after in &after_queue {
            if !prior_queue.contains_key(after.id.as_str()) {
                authority_changes.push(LocalExecutableChange {
                    doc_id: handle.chat_id.clone(),
                    kind: LocalExecutableKind::Queue,
                    row_id: after.id.clone(),
                    expected_hash: None,
                    replacement_hash: Some(executable_row_hash(after)?),
                    expected_workspace_repair_revision: None,
                    replacement_workspace_repair_revision: Some(admission_w),
                });
            }
        }
        let next_commands = after_commands
            .iter()
            .map(|row| (row.id.as_str(), row))
            .collect::<HashMap<_, _>>();
        for before in &before_commands {
            if next_commands.contains_key(before.id.as_str()) {
                continue;
            }
            if let Some(previous) = self.inner.store.local_executable_authority(
                &handle.chat_id,
                LocalExecutableKind::Command,
                &before.id,
            )? {
                authority_changes.push(LocalExecutableChange {
                    doc_id: handle.chat_id.clone(),
                    kind: LocalExecutableKind::Command,
                    row_id: before.id.clone(),
                    expected_hash: Some(previous.payload_hash),
                    replacement_hash: None,
                    expected_workspace_repair_revision: Some(previous.workspace_repair_revision),
                    replacement_workspace_repair_revision: None,
                });
            }
        }
        let prior_command_ids = before_commands
            .iter()
            .map(|row| row.id.as_str())
            .collect::<HashSet<_>>();
        for after in &after_commands {
            if prior_command_ids.contains(after.id.as_str())
                || after.status != SessionCommandStatus::Pending
            {
                continue;
            }
            let hash = executable_row_hash(after)?;
            let receipt = self.inner.store.task_command_receipt(&after.id)?;
            if receipt.as_ref().is_some_and(|receipt| {
                receipt.task_id == handle.chat_id
                    && matches!(
                        receipt.status,
                        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
                    )
            }) {
                authority_changes.push(LocalExecutableChange {
                    doc_id: handle.chat_id.clone(),
                    kind: LocalExecutableKind::Command,
                    row_id: after.id.clone(),
                    expected_hash: None,
                    replacement_hash: Some(hash),
                    expected_workspace_repair_revision: None,
                    replacement_workspace_repair_revision: self
                        .inner
                        .store
                        .command_workspace_repair_revision(&after.id)?,
                });
            } else {
                unreceipted_new_commands.push((after.id.clone(), hash));
            }
        }
        let snapshot = candidate.export_snapshot()?;
        let update = candidate
            .doc()
            .export(loro::ExportMode::updates(&frontier))
            .map_err(|error| EngineError::Other(format!("queue update export failed: {error}")))?;
        let batch_id =
            if !update.is_empty() && self.inner.store.chat_outbox_initialized(&handle.chat_id)? {
                Some(format!("queue-projection:{batch_key}"))
            } else {
                None
            };
        Ok(PreparedQueueProjection {
            outcome,
            snapshot,
            update,
            batch_id,
            authority_changes,
            unreceipted_new_commands,
        })
    }

    fn queue_projection_updates<'a, T>(
        handle: &'a ChatDocHandle,
        prepared: &'a PreparedQueueProjection<T>,
    ) -> Vec<(&'a str, &'a str, &'a [u8])> {
        prepared
            .batch_id
            .as_deref()
            .map(|batch_id| {
                vec![(
                    handle.chat_id.as_str(),
                    batch_id,
                    prepared.update.as_slice(),
                )]
            })
            .unwrap_or_default()
    }

    /// Install and publish only a projection that is already durable. Imported
    /// Loro updates do not echo through the local-update subscription; the
    /// pre-journaled batch is handed to the client with the same stable id.
    fn install_queue_projection<T>(
        &self,
        handle: &ChatDocHandle,
        prepared: PreparedQueueProjection<T>,
    ) -> Result<T, EngineError> {
        if !prepared.update.is_empty() {
            let status = handle.doc.doc().import(&prepared.update).map_err(|error| {
                EngineError::Other(format!("queue update install failed: {error}"))
            })?;
            if status.pending.is_some() {
                return Err(EngineError::Other(
                    "queue update install is missing causal dependencies".into(),
                ));
            }
        }
        handle
            .snapshot_bytes
            .store(prepared.snapshot.len(), Ordering::Release);
        if let Some(batch_id) = prepared.batch_id {
            let client = lock(&handle.chat2);
            match &*client {
                Some(client) => client.enqueue_batch(batch_id, prepared.update),
                None => lock(&handle.chat2_pending_local).push((batch_id, prepared.update)),
            }
        }
        handle.publish_queue();
        Ok(prepared.outcome)
    }

    fn persist_queue_projection<T>(
        &self,
        handle: &ChatDocHandle,
        batch_key: &str,
        mutation: impl FnOnce(&SessionDoc) -> Result<T, EngineError>,
    ) -> Result<T, EngineError> {
        self.persist_queue_projection_if(handle, batch_key, |doc| {
            mutation(doc).map(|outcome| (outcome, true))
        })
    }

    fn persist_queue_projection_if<T>(
        &self,
        handle: &ChatDocHandle,
        batch_key: &str,
        mutation: impl FnOnce(&SessionDoc) -> Result<(T, bool), EngineError>,
    ) -> Result<T, EngineError> {
        let _projection = lock(&handle.queue_projection_lock);
        let prepared = self.prepare_queue_projection(handle, batch_key, mutation)?;
        let (outcome, commit) = prepared.outcome;
        if !commit {
            return Ok(outcome);
        }
        let prepared = PreparedQueueProjection {
            outcome,
            snapshot: prepared.snapshot,
            update: prepared.update,
            batch_id: prepared.batch_id,
            authority_changes: prepared.authority_changes,
            unreceipted_new_commands: prepared.unreceipted_new_commands,
        };
        let update = prepared
            .batch_id
            .as_deref()
            .map(|batch_id| vec![(batch_id, prepared.update.as_slice())])
            .unwrap_or_default();
        self.inner.store.save_snapshot_with_updates_and_authority(
            &handle.chat_id,
            &prepared.snapshot,
            &update,
            &prepared.authority_changes,
        )?;
        self.install_queue_projection(handle, prepared)
    }

    fn commit_queue_task_projection<T>(
        &self,
        handle: &ChatDocHandle,
        command_id: &str,
        mutation: impl FnOnce(&SessionDoc) -> Result<(T, bool), EngineError>,
    ) -> Result<(T, Option<CommandReceipt>), EngineError> {
        let _projection = lock(&handle.queue_projection_lock);
        let prepared = self.prepare_queue_projection(handle, command_id, mutation)?;
        let (outcome, commit) = prepared.outcome;
        if !commit {
            return Ok((outcome, None));
        }
        let prepared = PreparedQueueProjection {
            outcome,
            snapshot: prepared.snapshot,
            update: prepared.update,
            batch_id: prepared.batch_id,
            authority_changes: prepared.authority_changes,
            unreceipted_new_commands: prepared.unreceipted_new_commands,
        };
        let pending = self
            .inner
            .store
            .task_command_receipt(command_id)?
            .ok_or_else(|| EngineError::Other("task command receipt vanished".into()))?;
        let snapshots = [(handle.chat_id.as_str(), prepared.snapshot.as_slice())];
        let updates = Self::queue_projection_updates(handle, &prepared);
        let receipt = self
            .inner
            .store
            .complete_task_command_with_snapshot_changes_updates_and_authority(
                command_id,
                CommandReceiptStatus::Applied,
                true,
                pending.snapshot.binding_id.as_deref(),
                pending.snapshot.binding_generation,
                pending.snapshot.active_turn_id.as_deref(),
                None,
                None,
                &snapshots,
                &[],
                &updates,
                &prepared.authority_changes,
            )?
            .ok_or_else(|| EngineError::Other("task command receipt vanished".into()))?;
        let outcome = self.install_queue_projection(handle, prepared)?;
        Ok((outcome, Some(receipt)))
    }

    /// The approved cross-task send is a product write. Keep an older live
    /// handle from saving over the atomic receipt/queue snapshot if storage
    /// succeeded but the live CRDT install did not. The callback makes that
    /// exact post-commit boundary testable without a provider or real data.
    fn commit_approved_queue_projection_after_commit<T>(
        &self,
        handle: &ChatDocHandle,
        command_id: &str,
        mutation: impl FnOnce(&SessionDoc) -> Result<(T, bool), EngineError>,
        after_commit: impl FnOnce() -> Result<(), EngineError>,
    ) -> Result<(T, Option<CommandReceipt>), TaskProjectionError> {
        let _projection = lock(&handle.queue_projection_lock);
        if handle
            .durable_projection_uninstalled
            .load(Ordering::Acquire)
        {
            return Err(TaskProjectionError::Mutation(EngineError::Other(
                "a committed document projection needs recovery after restart".into(),
            )));
        }
        let prepared = self
            .prepare_queue_projection(handle, command_id, mutation)
            .map_err(TaskProjectionError::Mutation)?;
        let (outcome, commit) = prepared.outcome;
        if !commit {
            return Ok((outcome, None));
        }
        let prepared = PreparedQueueProjection {
            outcome,
            snapshot: prepared.snapshot,
            update: prepared.update,
            batch_id: prepared.batch_id,
            authority_changes: prepared.authority_changes,
            unreceipted_new_commands: prepared.unreceipted_new_commands,
        };
        let pending = self
            .inner
            .store
            .task_command_receipt(command_id)
            .map_err(|error| TaskProjectionError::Mutation(error.into()))?
            .ok_or_else(|| {
                TaskProjectionError::Mutation(EngineError::Other(
                    "task command receipt vanished".into(),
                ))
            })?;
        if pending.status != CommandReceiptStatus::Pending {
            return Err(TaskProjectionError::Mutation(EngineError::Other(
                "approved queue command is no longer pending".into(),
            )));
        }
        let snapshots = [(handle.chat_id.as_str(), prepared.snapshot.as_slice())];
        let updates = Self::queue_projection_updates(handle, &prepared);
        // A storage error can be ambiguous. Fence stale saves before starting
        // the transaction, then clear only if a fresh receipt proves it did
        // not commit. Receipt, snapshot and outbox share that transaction.
        handle
            .durable_projection_uninstalled
            .store(true, Ordering::Release);
        let committed = self
            .inner
            .store
            .complete_task_command_with_snapshot_changes_updates_and_authority(
                command_id,
                CommandReceiptStatus::Applied,
                true,
                pending.snapshot.binding_id.as_deref(),
                pending.snapshot.binding_generation,
                pending.snapshot.active_turn_id.as_deref(),
                None,
                None,
                &snapshots,
                &[],
                &updates,
                &prepared.authority_changes,
            );
        let receipt = match committed {
            Ok(Some(receipt)) if receipt.status == CommandReceiptStatus::Applied => receipt,
            Ok(Some(_)) | Ok(None) => {
                handle
                    .durable_projection_uninstalled
                    .store(false, Ordering::Release);
                return Err(TaskProjectionError::Mutation(EngineError::Other(
                    "approved queue command did not return an applied receipt".into(),
                )));
            }
            Err(error) => {
                let precommit =
                    self.inner
                        .store
                        .task_command_receipt(command_id)
                        .is_ok_and(|receipt| {
                            receipt.is_some_and(|receipt| {
                                receipt.status == CommandReceiptStatus::Pending
                            })
                        });
                if precommit {
                    handle
                        .durable_projection_uninstalled
                        .store(false, Ordering::Release);
                    return Err(TaskProjectionError::Mutation(error.into()));
                }
                return Err(TaskProjectionError::Commit(error.into()));
            }
        };
        after_commit().map_err(TaskProjectionError::Commit)?;
        let outcome = self
            .install_queue_projection(handle, prepared)
            .map_err(TaskProjectionError::Commit)?;
        handle
            .durable_projection_uninstalled
            .store(false, Ordering::Release);
        Ok((outcome, Some(receipt)))
    }

    fn correlate_queue_task_projection<T>(
        &self,
        handle: &ChatDocHandle,
        command_id: &str,
        correlation_id: &str,
        admit_hash: Option<&str>,
        mutation: impl FnOnce(&SessionDoc) -> Result<(T, bool), EngineError>,
    ) -> Result<(T, Option<CommandReceipt>), EngineError> {
        let _projection = lock(&handle.queue_projection_lock);
        let mut prepared = self.prepare_queue_projection(handle, command_id, mutation)?;
        let (outcome, commit) = prepared.outcome;
        if !commit {
            return Ok((outcome, None));
        }
        if admit_hash.is_some() {
            let consent_w = self
                .inner
                .store
                .command_workspace_repair_revision(command_id)?
                .ok_or_else(|| {
                    EngineError::Other(
                        "explicit queue action has no original workspace authority".into(),
                    )
                })?;
            let raw = loro::LoroDoc::new();
            raw.import(&prepared.snapshot).map_err(|error| {
                EngineError::Other(format!("reviewed queue projection import failed: {error}"))
            })?;
            let claimed = SessionDoc::from_doc(raw)
                .read_queue()?
                .into_iter()
                .find(|row| row.id == correlation_id)
                .ok_or_else(|| EngineError::Other("reviewed queued row vanished".into()))?;
            let new_hash = executable_row_hash(&claimed)?;
            if let Some(change) = prepared.authority_changes.iter_mut().find(|change| {
                change.kind == LocalExecutableKind::Queue && change.row_id == correlation_id
            }) {
                change.replacement_hash = Some(new_hash);
                change.replacement_workspace_repair_revision = Some(consent_w);
            } else {
                let previous = self.inner.store.local_executable_authority(
                    &handle.chat_id,
                    LocalExecutableKind::Queue,
                    correlation_id,
                )?;
                prepared.authority_changes.push(LocalExecutableChange {
                    doc_id: handle.chat_id.clone(),
                    kind: LocalExecutableKind::Queue,
                    row_id: correlation_id.to_owned(),
                    expected_hash: previous.as_ref().map(|grant| grant.payload_hash.clone()),
                    replacement_hash: Some(new_hash),
                    expected_workspace_repair_revision: previous
                        .map(|grant| grant.workspace_repair_revision),
                    replacement_workspace_repair_revision: Some(consent_w),
                });
            }
        }
        let prepared = PreparedQueueProjection {
            outcome,
            snapshot: prepared.snapshot,
            update: prepared.update,
            batch_id: prepared.batch_id,
            authority_changes: prepared.authority_changes,
            unreceipted_new_commands: prepared.unreceipted_new_commands,
        };
        let snapshots = [(handle.chat_id.as_str(), prepared.snapshot.as_slice())];
        let updates = Self::queue_projection_updates(handle, &prepared);
        let receipt = self
            .inner
            .store
            .set_task_command_correlation_with_snapshot_changes_and_authority(
                command_id,
                correlation_id,
                &snapshots,
                &updates,
                &prepared.authority_changes,
            )?
            .ok_or_else(|| EngineError::Other("task command receipt vanished".into()))?;
        let outcome = self.install_queue_projection(handle, prepared)?;
        self.publish_task_receipt(receipt.clone());
        Ok((outcome, Some(receipt)))
    }

    fn mark_queue_task_unknown<T>(
        &self,
        handle: &ChatDocHandle,
        command_id: &str,
        message: &str,
        mutation: impl FnOnce(&SessionDoc) -> Result<T, EngineError>,
    ) -> Result<(T, CommandReceipt), EngineError> {
        let _projection = lock(&handle.queue_projection_lock);
        let prepared = self.prepare_queue_projection(handle, command_id, mutation)?;
        let snapshots = [(handle.chat_id.as_str(), prepared.snapshot.as_slice())];
        let updates = Self::queue_projection_updates(handle, &prepared);
        let receipt = self
            .inner
            .store
            .mark_task_command_unknown_with_snapshot_changes_and_authority(
                command_id,
                message,
                &snapshots,
                &updates,
                &prepared.authority_changes,
            )?
            .ok_or_else(|| EngineError::Other("task command receipt vanished".into()))?;
        let outcome = self.install_queue_projection(handle, prepared)?;
        self.publish_task_receipt(receipt.clone());
        Ok((outcome, receipt))
    }

    fn reject_queue_task(
        &self,
        handle: &ChatDocHandle,
        message_id: &str,
        command_id: &str,
        hold_reason: QueueHoldReason,
    ) -> Result<QueuedMessage, EngineError> {
        let _projection = lock(&handle.queue_projection_lock);
        let prepared = self.prepare_queue_projection(handle, command_id, |doc| {
            doc.reject_queued_steer_claim(message_id, command_id, hold_reason)?
                .ok_or_else(|| EngineError::Other("queued steer claim vanished".into()))
        })?;
        let pending = self
            .inner
            .store
            .task_command_receipt(command_id)?
            .ok_or_else(|| EngineError::Other("task command receipt vanished".into()))?;
        let snapshots = [(handle.chat_id.as_str(), prepared.snapshot.as_slice())];
        let updates = Self::queue_projection_updates(handle, &prepared);
        let (reason_message, recovery_action) = match hold_reason {
            QueueHoldReason::ProviderRejected => (
                "the provider rejected native in-turn steering",
                "The message remains queued; retry only after a new active turn is visible.",
            ),
            QueueHoldReason::SteeringUnavailable => (
                "native steering is unavailable while another steer remains unconfirmed",
                "The message remains queued; wait for the current steer to settle before retrying.",
            ),
            _ => (
                "native in-turn steering is unavailable",
                "The message remains queued; review its hold reason before retrying.",
            ),
        };
        let receipt = self
            .inner
            .store
            .complete_task_command_with_snapshot_changes_updates_and_authority(
                command_id,
                CommandReceiptStatus::Rejected,
                false,
                pending.snapshot.binding_id.as_deref(),
                pending.snapshot.binding_generation,
                pending.snapshot.active_turn_id.as_deref(),
                Some(CommandReceiptReason {
                    code: CommandReasonCode::OperationRejected,
                    message: reason_message.into(),
                }),
                Some(recovery_action),
                &snapshots,
                &[],
                &updates,
                &prepared.authority_changes,
            )?
            .ok_or_else(|| EngineError::Other("task command receipt vanished".into()))?;
        let row = self.install_queue_projection(handle, prepared)?;
        self.publish_task_receipt(receipt);
        Ok(row)
    }

    /// A provider run ended after an explicit steer was handed to the adapter
    /// but before provider acceptance was confirmed. Preserve the claimed row
    /// as recoverable/unknown; never turn it into a fresh turn automatically.
    pub fn mark_steer_correlation_unknown(
        &self,
        chat_id: &str,
        message_id: &str,
    ) -> Result<(), EngineError> {
        let handle = self.open(chat_id)?;
        let Some(command_id) = handle
            .doc
            .read_queue()?
            .into_iter()
            .find(|item| item.id == message_id)
            .and_then(|item| match item.status {
                QueueMessageStatus::Claimed { command_id, .. }
                | QueueMessageStatus::SteeringAccepted { command_id, .. } => Some(command_id),
                _ => None,
            })
        else {
            return Ok(());
        };
        self.mark_queue_task_unknown(
            &handle,
            &command_id,
            "the provider ended before native steer recording could be confirmed",
            |doc| {
                doc.mark_queued_claim_unknown(message_id, &command_id, now_ms())?;
                Ok(())
            },
        )?;
        Ok(())
    }

    /// Hold a message for later: append it to the doc's queue. Any device may
    /// write here (unlike `messages`), and the change subscription kicks
    /// [`Self::drain_queue`], so a queue that lands while the agent is already
    /// idle goes straight out instead of waiting for a turn that never comes.
    pub fn queue_message(
        &self,
        chat_id: &str,
        text: &str,
        attachments: Vec<String>,
    ) -> Result<String, EngineError> {
        self.queue_message_with_behavior(chat_id, text, attachments, false)
    }

    /// Append a message while preserving the submitter's active-turn policy
    /// on the synchronized row. This matters when another device hosts the
    /// chat: the host, not the submitting UI, decides when to drain it.
    pub fn queue_message_with_behavior(
        &self,
        chat_id: &str,
        text: &str,
        attachments: Vec<String>,
        hold_for_turn_end: bool,
    ) -> Result<String, EngineError> {
        let id = new_id();
        self.queue_message_with_id_and_execution(
            chat_id,
            &id,
            text,
            attachments,
            hold_for_turn_end,
            luna_proto::FollowUpBehavior::Queue,
            None,
        )?;
        Ok(id)
    }

    /// L09 authoritative enqueue. `message_id` is also the stable queue row
    /// identity, so an L07 retry after response loss cannot append twice.
    pub fn queue_message_with_id_and_execution(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
        attachments: Vec<String>,
        hold_for_turn_end: bool,
        follow_up_behavior: luna_proto::FollowUpBehavior,
        execution: Option<luna_proto::ChatConfig>,
    ) -> Result<bool, EngineError> {
        self.queue_message_with_inputs(
            chat_id,
            message_id,
            text,
            attachments,
            Vec::new(),
            hold_for_turn_end,
            follow_up_behavior,
            execution,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn queue_message_with_inputs(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
        attachments: Vec<String>,
        input_items: Vec<luna_proto::ComposerInput>,
        hold_for_turn_end: bool,
        follow_up_behavior: luna_proto::FollowUpBehavior,
        execution: Option<luna_proto::ChatConfig>,
    ) -> Result<bool, EngineError> {
        let handle = self.open(chat_id)?;
        let inserted = self.persist_queue_projection(
            &handle,
            &format!("legacy-enqueue:{message_id}"),
            |doc| {
                if let Some(existing) = doc
                    .read_queue()?
                    .into_iter()
                    .find(|item| item.id == message_id)
                {
                    return Ok(existing.text == text
                        && existing.attachments == attachments
                        && existing.input_items == input_items
                        && existing.hold_for_turn_end == hold_for_turn_end
                        && existing.follow_up_behavior == follow_up_behavior
                        && existing.execution == execution);
                }
                doc.push_queued(&QueuedMessage {
                    input_items,
                    id: message_id.to_string(),
                    text: text.to_string(),
                    attachments,
                    hold_for_turn_end,
                    follow_up_behavior,
                    hold_reason: None,
                    issued_by: self.inner.config.device_id.clone(),
                    issued_at: now_ms(),
                    edited_at: None,
                    revision: 1,
                    execution,
                    status: QueueMessageStatus::Held,
                    delivery_gate: None,
                })?;
                Ok(true)
            },
        )?;
        // Same reasoning as a command: the user is acting in this chat again.
        if inserted {
            self.unarchive_on_send(chat_id);
            self.nudge_remote_host(chat_id);
        }
        Ok(inserted)
    }

    pub async fn enqueue_queued_message(
        &self,
        command_id: &str,
        chat_id: &str,
        message_id: &str,
        text: &str,
        attachments: Vec<String>,
        hold_for_turn_end: bool,
        follow_up_behavior: luna_proto::FollowUpBehavior,
        execution: Option<luna_proto::ChatConfig>,
    ) -> Result<(QueueMutationOutcome, Option<CommandReceipt>), EngineError> {
        self.enqueue_queued_message_guarded(
            command_id,
            chat_id,
            message_id,
            text,
            attachments,
            Vec::new(),
            hold_for_turn_end,
            follow_up_behavior,
            execution,
            None,
        )
        .await
        .map_err(|error| EngineError::Other(error.message().to_string()))
    }

    #[allow(clippy::too_many_arguments)] // Mirrors the existing queue service with one source fence.
    pub(crate) async fn enqueue_queued_message_guarded(
        &self,
        command_id: &str,
        chat_id: &str,
        message_id: &str,
        text: &str,
        attachments: Vec<String>,
        input_items: Vec<luna_proto::ComposerInput>,
        hold_for_turn_end: bool,
        follow_up_behavior: luna_proto::FollowUpBehavior,
        execution: Option<luna_proto::ChatConfig>,
        source: Option<(&SessionsEngine, &crate::cross_task_tools::ApprovedSendFence)>,
    ) -> Result<(QueueMutationOutcome, Option<CommandReceipt>), TaskProjectionError> {
        if !self.is_host(chat_id) {
            return Err(TaskProjectionError::Mutation(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            ))));
        }
        let handle = self.open(chat_id).map_err(TaskProjectionError::Mutation)?;
        let _drain = handle.drain_lock.lock().await;
        let approved = source.is_some();
        let commit = || {
            let mutation = |doc: &SessionDoc| {
                if let Some(existing) = doc
                    .read_queue()?
                    .into_iter()
                    .find(|item| item.id == message_id)
                {
                    return Ok(
                        if existing.text == text
                            && existing.attachments == attachments
                            && existing.input_items == input_items
                            && existing.hold_for_turn_end == hold_for_turn_end
                            && existing.follow_up_behavior == follow_up_behavior
                            && existing.execution == execution
                        {
                            (QueueMutationOutcome::Duplicate, true)
                        } else {
                            (QueueMutationOutcome::RevisionConflict(existing), false)
                        },
                    );
                }
                doc.push_queued(&QueuedMessage {
                    input_items,
                    id: message_id.to_string(),
                    text: text.to_string(),
                    attachments,
                    hold_for_turn_end,
                    follow_up_behavior,
                    hold_reason: None,
                    issued_by: self.inner.config.device_id.clone(),
                    issued_at: now_ms(),
                    edited_at: None,
                    revision: 1,
                    execution,
                    status: QueueMessageStatus::Held,
                    delivery_gate: None,
                })?;
                Ok((QueueMutationOutcome::Applied, true))
            };
            if approved {
                self.commit_approved_queue_projection_after_commit(
                    &handle,
                    command_id,
                    mutation,
                    || {
                        #[cfg(test)]
                        if self
                            .inner
                            .approved_queue_postcommit_fail_once
                            .swap(false, Ordering::AcqRel)
                        {
                            return Err(EngineError::Other(
                                "synthetic approved queue install failure".into(),
                            ));
                        }
                        Ok(())
                    },
                )
            } else {
                self.commit_queue_task_projection(&handle, command_id, mutation)
                    .map_err(TaskProjectionError::Mutation)
            }
        };
        let result = match source {
            Some((sessions, fence)) => sessions
                .with_live_approved_send(fence, || Ok(commit()))
                .map_err(TaskProjectionError::Mutation)?,
            None => commit(),
        }?;
        if result.1.is_some() {
            self.unarchive_on_send(chat_id);
        }
        Ok(result)
    }

    pub fn queued_message(
        &self,
        chat_id: &str,
        id: &str,
    ) -> Result<Option<QueuedMessage>, EngineError> {
        Ok(self
            .open(chat_id)?
            .doc
            .read_queue()?
            .into_iter()
            .find(|item| item.id == id))
    }

    /// Retype a queued message. Empty text deletes the row — emptying the box
    /// is how you say "drop it". `false` when the row is already gone.
    pub fn update_queued_message(
        &self,
        chat_id: &str,
        id: &str,
        text: &str,
    ) -> Result<bool, EngineError> {
        if !self.is_host(chat_id) {
            return Err(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            )));
        }
        let handle = self.open(chat_id)?;
        self.persist_queue_projection_if(
            &handle,
            &format!("legacy-edit:{}", uuid::Uuid::new_v4()),
            |doc| {
                let changed = doc.set_queued_text(id, text, now_ms())?;
                Ok((changed, changed))
            },
        )
    }

    /// Reorder a queued message (drag, or the up/down buttons).
    pub fn move_queued_message(
        &self,
        chat_id: &str,
        id: &str,
        to_index: usize,
    ) -> Result<bool, EngineError> {
        if !self.is_host(chat_id) {
            return Err(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            )));
        }
        let handle = self.open(chat_id)?;
        self.persist_queue_projection_if(
            &handle,
            &format!("move:{}", uuid::Uuid::new_v4()),
            |doc| {
                let changed = doc.move_queued(id, to_index)?;
                Ok((changed, changed))
            },
        )
    }

    /// Cancel one queued message at the chat host. Removal shares the same
    /// lock as automatic and explicit delivery, so the acknowledgement is the
    /// linearization point: `true` guarantees this host did not take the row.
    pub async fn remove_queued_message(
        &self,
        chat_id: &str,
        id: &str,
    ) -> Result<bool, EngineError> {
        if !self.is_host(chat_id) {
            return Err(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            )));
        }
        let handle = self.open(chat_id)?;
        let _drain = handle.drain_lock.lock().await;
        self.persist_queue_projection_if(
            &handle,
            &format!("legacy-remove:{}", uuid::Uuid::new_v4()),
            |doc| {
                let removed = doc.remove_queued(id)?;
                Ok((removed, removed))
            },
        )
    }

    pub async fn remove_queued_message_at_revision(
        &self,
        command_id: &str,
        chat_id: &str,
        id: &str,
        expected_revision: u64,
    ) -> Result<(QueueMutationOutcome, Option<CommandReceipt>), EngineError> {
        if !self.is_host(chat_id) {
            return Err(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            )));
        }
        let handle = self.open(chat_id)?;
        let _drain = handle.drain_lock.lock().await;
        self.commit_queue_task_projection(&handle, command_id, |doc| {
            let Some(item) = doc.read_queue()?.into_iter().find(|item| item.id == id) else {
                // Removal is idempotent. The same command may have committed
                // before an older build lost its receipt write.
                return Ok((QueueMutationOutcome::Missing, true));
            };
            if !matches!(item.status, QueueMessageStatus::Held) {
                return Ok((QueueMutationOutcome::Claimed(item), false));
            }
            if item.revision != expected_revision {
                return Ok((QueueMutationOutcome::RevisionConflict(item), false));
            }
            if !doc.remove_queued(id)? {
                return Ok((QueueMutationOutcome::Missing, true));
            }
            Ok((QueueMutationOutcome::Applied, true))
        })
    }

    /// Acquire the host-side right to edit one queued row. This operation and
    /// every queue take share `drain_lock`, making the ACK the linearization
    /// point: after Acquired the row cannot race into the agent.
    pub async fn begin_queued_message_edit(
        &self,
        chat_id: &str,
        id: &str,
        owner_device_id: &str,
        owner_instance_id: &str,
    ) -> Result<BeginQueueEditOutcome, EngineError> {
        if !self.is_host(chat_id) {
            return Err(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            )));
        }
        let handle = self.open(chat_id)?;
        let _drain = handle.drain_lock.lock().await;
        let now = now_ms();
        let lease_id = new_id();
        let expires_at_ms = now + QUEUE_EDIT_LEASE_MS;
        let outcome =
            self.persist_queue_projection_if(&handle, &format!("edit-begin:{lease_id}"), |doc| {
                let Some(item) = doc.read_queue()?.into_iter().find(|item| item.id == id) else {
                    return Ok((BeginQueueEditOutcome::Missing, false));
                };
                match &item.status {
                    QueueMessageStatus::Held => {}
                    QueueMessageStatus::Claimed { command_id, .. }
                    | QueueMessageStatus::SteeringAccepted { command_id, .. }
                    | QueueMessageStatus::DeliveryUnknown { command_id, .. } => {
                        return Ok((
                            BeginQueueEditOutcome::Claimed {
                                command_id: command_id.clone(),
                            },
                            false,
                        ));
                    }
                }
                if let Some(QueueDeliveryGate::Editing {
                    owner_device_id,
                    expires_at_ms,
                    ..
                }) = &item.delivery_gate
                    && *expires_at_ms > now
                {
                    return Ok((
                        BeginQueueEditOutcome::Locked {
                            owner_device_id: owner_device_id.clone(),
                            expires_at_ms: *expires_at_ms,
                        },
                        false,
                    ));
                }
                let base_text_hash = queue_text_hash(&item.text);
                let gate = QueueDeliveryGate::Editing {
                    lease_id: lease_id.clone(),
                    owner_device_id: owner_device_id.to_string(),
                    owner_instance_id: owner_instance_id.to_string(),
                    acquired_at_ms: now,
                    expires_at_ms,
                    base_text_hash: base_text_hash.clone(),
                };
                if !doc.set_queued_delivery_gate(id, Some(&gate))? {
                    return Ok((BeginQueueEditOutcome::Missing, false));
                }
                Ok((
                    BeginQueueEditOutcome::Acquired {
                        lease_id: lease_id.clone(),
                        text: item.text,
                        attachments: item.attachments,
                        input_items: item.input_items,
                        base_text_hash,
                        revision: item.revision,
                        expires_at_ms,
                    },
                    true,
                ))
            })?;
        if matches!(outcome, BeginQueueEditOutcome::Acquired { .. }) {
            self.arm_queue_edit_expiry(&handle, id, &lease_id, expires_at_ms);
        }
        Ok(outcome)
    }

    /// Extend an edit lease. An already-expired generation is never revived;
    /// it remains blocked and will be surfaced as ReviewRequired.
    pub async fn renew_queued_message_edit(
        &self,
        chat_id: &str,
        id: &str,
        lease_id: &str,
    ) -> Result<RenewQueueEditOutcome, EngineError> {
        if !self.is_host(chat_id) {
            return Err(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            )));
        }
        let handle = self.open(chat_id)?;
        let _drain = handle.drain_lock.lock().await;
        let now = now_ms();
        let renewed_expires_at_ms = now + QUEUE_EDIT_LEASE_MS;
        let outcome = self.persist_queue_projection_if(
            &handle,
            &format!("edit-renew:{}", uuid::Uuid::new_v4()),
            |doc| {
                let Some(item) = doc.read_queue()?.into_iter().find(|item| item.id == id) else {
                    return Ok((RenewQueueEditOutcome::Missing, false));
                };
                let QueueDeliveryGate::Editing {
                    lease_id: current,
                    owner_device_id,
                    owner_instance_id,
                    acquired_at_ms,
                    expires_at_ms,
                    base_text_hash,
                } = item
                    .delivery_gate
                    .unwrap_or(QueueDeliveryGate::ReviewRequired {
                        previous_lease_id: String::new(),
                        owner_device_id: String::new(),
                        since_ms: now,
                        base_text_hash: String::new(),
                    })
                else {
                    return Ok((RenewQueueEditOutcome::Lost, false));
                };
                if current != lease_id || expires_at_ms <= now {
                    if current == lease_id && expires_at_ms <= now {
                        let review = QueueDeliveryGate::ReviewRequired {
                            previous_lease_id: current,
                            owner_device_id,
                            since_ms: now,
                            base_text_hash,
                        };
                        let changed = doc.set_queued_delivery_gate(id, Some(&review))?;
                        return Ok((RenewQueueEditOutcome::Lost, changed));
                    }
                    return Ok((RenewQueueEditOutcome::Lost, false));
                }
                let renewed = QueueDeliveryGate::Editing {
                    lease_id: current,
                    owner_device_id,
                    owner_instance_id,
                    acquired_at_ms,
                    expires_at_ms: renewed_expires_at_ms,
                    base_text_hash,
                };
                if !doc.set_queued_delivery_gate(id, Some(&renewed))? {
                    return Ok((RenewQueueEditOutcome::Missing, false));
                }
                Ok((
                    RenewQueueEditOutcome::Renewed {
                        expires_at_ms: renewed_expires_at_ms,
                    },
                    true,
                ))
            },
        )?;
        if matches!(outcome, RenewQueueEditOutcome::Renewed { .. }) {
            self.arm_queue_edit_expiry(&handle, id, lease_id, renewed_expires_at_ms);
        }
        Ok(outcome)
    }

    /// Resolve an edit lease. A late finish may still resolve the matching
    /// ReviewRequired generation, but can never affect a newer lease.
    pub async fn finish_queued_message_edit(
        &self,
        chat_id: &str,
        id: &str,
        lease_id: &str,
        action: FinishQueueEditAction,
        text: Option<&str>,
        expected_text_hash: Option<&str>,
    ) -> Result<FinishQueueEditOutcome, EngineError> {
        self.finish_queued_message_edit_with_attachments(
            chat_id,
            id,
            lease_id,
            action,
            text,
            expected_text_hash,
            None,
        )
        .await
    }

    pub async fn finish_queued_message_edit_with_attachments(
        &self,
        chat_id: &str,
        id: &str,
        lease_id: &str,
        action: FinishQueueEditAction,
        text: Option<&str>,
        expected_text_hash: Option<&str>,
        attachments: Option<&[String]>,
    ) -> Result<FinishQueueEditOutcome, EngineError> {
        self.finish_queued_message_edit_with_attachments_at_revision(
            chat_id,
            id,
            lease_id,
            action,
            text,
            expected_text_hash,
            attachments,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn finish_queued_message_edit_with_attachments_at_revision(
        &self,
        chat_id: &str,
        id: &str,
        lease_id: &str,
        action: FinishQueueEditAction,
        text: Option<&str>,
        expected_text_hash: Option<&str>,
        attachments: Option<&[String]>,
        expected_revision: Option<u64>,
    ) -> Result<FinishQueueEditOutcome, EngineError> {
        Ok(self
            .finish_queued_message_edit_durable(
                None,
                chat_id,
                id,
                lease_id,
                action,
                text,
                expected_text_hash,
                attachments,
                None,
                expected_revision,
            )
            .await?
            .0)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn finish_queued_message_edit_task(
        &self,
        command_id: &str,
        chat_id: &str,
        id: &str,
        lease_id: &str,
        text: &str,
        expected_text_hash: &str,
        attachments: &[String],
        expected_revision: u64,
    ) -> Result<(FinishQueueEditOutcome, Option<CommandReceipt>), EngineError> {
        self.finish_queued_message_edit_task_with_inputs(
            command_id,
            chat_id,
            id,
            lease_id,
            text,
            expected_text_hash,
            attachments,
            None,
            expected_revision,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn finish_queued_message_edit_task_with_inputs(
        &self,
        command_id: &str,
        chat_id: &str,
        id: &str,
        lease_id: &str,
        text: &str,
        expected_text_hash: &str,
        attachments: &[String],
        input_items: Option<&[luna_proto::ComposerInput]>,
        expected_revision: u64,
    ) -> Result<(FinishQueueEditOutcome, Option<CommandReceipt>), EngineError> {
        self.finish_queued_message_edit_durable(
            Some(command_id),
            chat_id,
            id,
            lease_id,
            FinishQueueEditAction::Commit,
            Some(text),
            Some(expected_text_hash),
            Some(attachments),
            input_items,
            Some(expected_revision),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_queued_message_edit_durable(
        &self,
        command_id: Option<&str>,
        chat_id: &str,
        id: &str,
        lease_id: &str,
        action: FinishQueueEditAction,
        text: Option<&str>,
        expected_text_hash: Option<&str>,
        attachments: Option<&[String]>,
        input_items: Option<&[luna_proto::ComposerInput]>,
        expected_revision: Option<u64>,
    ) -> Result<(FinishQueueEditOutcome, Option<CommandReceipt>), EngineError> {
        if !self.is_host(chat_id) {
            return Err(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            )));
        }
        if action == FinishQueueEditAction::Commit
            && (text.is_none() || expected_text_hash.is_none())
        {
            return Err(EngineError::Other(
                "commit requires text and expectedTextHash".into(),
            ));
        }
        let handle = self.open(chat_id)?;
        let result = {
            let _drain = handle.drain_lock.lock().await;
            if let Some(command_id) = command_id {
                self.commit_queue_task_projection(&handle, command_id, |doc| {
                    finish_queue_edit_projection(
                        doc,
                        id,
                        lease_id,
                        action,
                        text,
                        expected_text_hash,
                        attachments,
                        input_items,
                        expected_revision,
                    )
                })?
            } else {
                let outcome = self.persist_queue_projection(
                    &handle,
                    &format!("edit:{}", uuid::Uuid::new_v4()),
                    |doc| {
                        finish_queue_edit_projection(
                            doc,
                            id,
                            lease_id,
                            action,
                            text,
                            expected_text_hash,
                            attachments,
                            input_items,
                            expected_revision,
                        )
                        .map(|(outcome, _)| outcome)
                    },
                )?;
                (outcome, None)
            }
        };
        // Turn-end may already have happened while the edit was open.
        self.drain_queue(&handle).await;
        Ok(result)
    }

    fn arm_existing_queue_edit_expiries(&self, handle: &Arc<ChatDocHandle>) {
        let Ok(queue) = handle.doc.read_queue() else {
            return;
        };
        for item in queue {
            if let Some(QueueDeliveryGate::Editing {
                lease_id,
                expires_at_ms,
                ..
            }) = item.delivery_gate
            {
                self.arm_queue_edit_expiry(handle, &item.id, &lease_id, expires_at_ms);
            }
        }
    }

    fn arm_queue_edit_expiry(
        &self,
        handle: &Arc<ChatDocHandle>,
        id: &str,
        lease_id: &str,
        expires_at_ms: i64,
    ) {
        let delay_ms = expires_at_ms.saturating_sub(now_ms()).max(0) as u64;
        let host = self.clone();
        let handle = handle.clone();
        let id = id.to_string();
        let lease_id = lease_id.to_string();
        self.spawn_worker(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            host.expire_queued_message_edit(&handle, &id, &lease_id, expires_at_ms)
                .await;
        });
    }

    /// Expire exactly the lease generation that scheduled this wake. A stale
    /// timer from before a renewal observes a different deadline and no-ops;
    /// timers for other rows are completely independent.
    async fn expire_queued_message_edit(
        &self,
        handle: &Arc<ChatDocHandle>,
        id: &str,
        lease_id: &str,
        scheduled_expires_at_ms: i64,
    ) {
        let changed = {
            let _drain = handle.drain_lock.lock().await;
            match self.persist_queue_projection_if(
                handle,
                &format!("edit-expire:{}", uuid::Uuid::new_v4()),
                |doc| {
                    let Some(item) = doc.read_queue()?.into_iter().find(|item| item.id == id)
                    else {
                        return Ok((false, false));
                    };
                    let Some(QueueDeliveryGate::Editing {
                        lease_id: current_lease_id,
                        owner_device_id,
                        expires_at_ms,
                        base_text_hash,
                        ..
                    }) = item.delivery_gate
                    else {
                        return Ok((false, false));
                    };
                    if current_lease_id != lease_id
                        || expires_at_ms != scheduled_expires_at_ms
                        || expires_at_ms > now_ms()
                    {
                        return Ok((false, false));
                    }
                    let review = QueueDeliveryGate::ReviewRequired {
                        previous_lease_id: current_lease_id,
                        owner_device_id,
                        since_ms: now_ms(),
                        base_text_hash,
                    };
                    let changed = doc.set_queued_delivery_gate(id, Some(&review))?;
                    Ok((changed, changed))
                },
            ) {
                Ok(changed) => changed,
                Err(error) => {
                    tracing::warn!(chat = %handle.chat_id, %error, "queue edit expiry persist failed");
                    false
                }
            }
        };
        if changed {
            // If this was the head, the drain now observes ReviewRequired. If
            // it was not, publishing still updates every client's row state.
            self.drain_queue(handle).await;
        }
    }

    /// "Send this one now": take it out of the queue and put it in front of the
    /// agent, interrupting whatever is running. Deliberately blunt — it is the
    /// explicit override. The empty-composer Enter gesture reaches this path
    /// only when the selected provider cannot steer the row. `false` when
    /// another device already took it.
    pub async fn send_queued_now(&self, chat_id: &str, id: &str) -> Result<bool, EngineError> {
        let Some(item) = self.queued_message(chat_id, id)? else {
            return Ok(false);
        };
        if item.delivery_gate.is_some() {
            return Err(EngineError::Other(
                "queued message is blocked for editing or review".into(),
            ));
        }
        let command_id = self.begin_legacy_queue_delivery(chat_id, &item, "sendQueuedNow")?;
        Ok(matches!(
            self.send_queued_now_with_claim(chat_id, id, item.revision, &command_id, None)
                .await?,
            QueueDispatchOutcome::Started | QueueDispatchOutcome::AlreadyClaimed(_)
        ))
    }

    fn begin_legacy_queue_delivery(
        &self,
        chat_id: &str,
        item: &QueuedMessage,
        intent: &str,
    ) -> Result<String, EngineError> {
        let command_id = format!("legacy-{intent}-{}", uuid::Uuid::new_v4());
        let snapshot = self.task_command_snapshot(chat_id)?.snapshot;
        let now = now_ms();
        let pending = CommandReceipt {
            command_id: command_id.clone(),
            task_id: chat_id.to_string(),
            intent: intent.to_string(),
            payload_hash: format!(
                "{:x}",
                Sha256::digest(format!("{}:{}:{}", item.id, item.revision, item.text).as_bytes())
            ),
            status: CommandReceiptStatus::Pending,
            snapshot,
            event_sequence: None,
            actor_id: self.inner.config.device_id.clone(),
            client_id: "legacy-queue-rpc".into(),
            client_session_id: "engine".into(),
            execution_environment: Some(self.inner.config.device_id.clone()),
            correlation_id: Some(item.id.clone()),
            reason: None,
            recovery_action: None,
            accepted_at_ms: now,
            committed_at_ms: None,
            observed_at_ms: now,
        };
        let begin = self.begin_task_command(pending, None, None, None)?;
        if begin.kind != luna_sync::TaskCommandBeginKind::Accepted {
            return Err(EngineError::Other(
                "legacy queue delivery command id unexpectedly collided".into(),
            ));
        }
        self.publish_task_receipt(begin.receipt);
        Ok(command_id)
    }

    /// L09 claim-first explicit delivery. The claimed row stays in the
    /// durable queue projection until `confirm_task_command_correlation`
    /// observes the matching provider message/turn. A replay that finds its
    /// own claim never calls the provider again.
    pub async fn send_queued_now_with_claim(
        &self,
        chat_id: &str,
        id: &str,
        expected_revision: u64,
        command_id: &str,
        expected_turn_id: Option<&str>,
    ) -> Result<QueueDispatchOutcome, EngineError> {
        self.dispatch_queued_with_claim(
            chat_id,
            id,
            expected_revision,
            command_id,
            QueueClaimMode::SendNow,
            QueueSend::Interrupt,
            expected_turn_id,
        )
        .await
    }

    pub async fn steer_queued_now_with_claim(
        &self,
        chat_id: &str,
        id: &str,
        expected_revision: u64,
        command_id: &str,
        expected_turn_id: Option<&str>,
    ) -> Result<QueueDispatchOutcome, EngineError> {
        self.dispatch_queued_with_claim(
            chat_id,
            id,
            expected_revision,
            command_id,
            QueueClaimMode::SteerNow,
            QueueSend::Steer,
            expected_turn_id,
        )
        .await
    }

    async fn dispatch_queued_with_claim(
        &self,
        chat_id: &str,
        id: &str,
        expected_revision: u64,
        command_id: &str,
        mode: QueueClaimMode,
        send: QueueSend,
        expected_turn_id: Option<&str>,
    ) -> Result<QueueDispatchOutcome, EngineError> {
        if !self.is_host(chat_id) {
            return Err(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            )));
        }
        let handle = self.open(chat_id)?;
        let _drain = handle.drain_lock.lock().await;
        if send == QueueSend::Steer {
            let Some(candidate) = handle
                .doc
                .read_queue()?
                .into_iter()
                .find(|item| item.id == id)
            else {
                return Ok(QueueDispatchOutcome::Missing);
            };
            let reason = if candidate.follow_up_behavior != luna_proto::FollowUpBehavior::Steer {
                Some(QueueHoldReason::SteeringUnavailable)
            } else if !candidate.attachments.is_empty()
                && candidate
                    .execution
                    .as_ref()
                    .map(|c| c.harness)
                    .unwrap_or_else(|| self.harness_for(chat_id))
                    != luna_proto::HarnessId::Codex
            {
                Some(QueueHoldReason::AttachmentsUnsupported)
            } else {
                let Some(sessions) = self.sessions() else {
                    return Err(EngineError::Other("sessions engine not wired".into()));
                };
                if expected_turn_id.is_some()
                    && sessions.active_turn_id(chat_id).as_deref() != expected_turn_id
                {
                    Some(QueueHoldReason::StaleTurn)
                } else {
                    match sessions
                        .session_status(chat_id)
                        .map(|session| session.status)
                    {
                        Some(luna_proto::SessionStatus::Working) => {
                            let harness = candidate
                                .execution
                                .as_ref()
                                .map(|execution| execution.harness)
                                .unwrap_or_else(|| self.harness_for(chat_id));
                            (!sessions.steers_mid_turn(harness))
                                .then_some(QueueHoldReason::SteeringUnavailable)
                        }
                        Some(luna_proto::SessionStatus::AwaitingApproval) => {
                            Some(QueueHoldReason::AwaitingApproval)
                        }
                        Some(luna_proto::SessionStatus::AwaitingInput) => {
                            Some(QueueHoldReason::AwaitingInput)
                        }
                        _ => Some(QueueHoldReason::SteeringUnavailable),
                    }
                }
            };
            if let Some(reason) = reason {
                let held = self.persist_queue_projection(
                    &handle,
                    &format!("steer-hold:{id}:{}", uuid::Uuid::new_v4()),
                    |doc| {
                        doc.set_queued_hold_reason(id, reason)?.ok_or_else(|| {
                            EngineError::Other("queued message is not steerable".into())
                        })
                    },
                )?;
                return Ok(QueueDispatchOutcome::Blocked(held));
            }
        }
        if expected_turn_id.is_some() && handle.queue_paused.load(Ordering::Acquire) {
            let item = handle
                .doc
                .read_queue()?
                .into_iter()
                .find(|item| item.id == id);
            return Ok(item
                .map(QueueDispatchOutcome::Blocked)
                .unwrap_or(QueueDispatchOutcome::Missing));
        }
        if let Some(expected_turn_id) = expected_turn_id {
            let Some(sessions) = self.sessions() else {
                return Err(EngineError::Other("sessions engine not wired".into()));
            };
            if sessions.active_turn_id(chat_id).as_deref() != Some(expected_turn_id) {
                let item = handle
                    .doc
                    .read_queue()?
                    .into_iter()
                    .find(|item| item.id == id);
                return Ok(item
                    .map(QueueDispatchOutcome::Blocked)
                    .unwrap_or(QueueDispatchOutcome::Missing));
            }
        }
        if self.request_from_chat_row(chat_id, "").is_none()
            && self
                .sessions()
                .and_then(|sessions| sessions.last_request(chat_id))
                .is_none()
        {
            return Err(EngineError::Other(
                "no live run and no prior run config".into(),
            ));
        }
        // An explicit local Send/Steer action may adopt a visible legacy or
        // quarantined row. The durable pending receipt is the local consent;
        // never infer it from the row's `issued_by` or relay route. Keep the
        // drain lock through the claim so automatic delivery cannot race this
        // one-use review decision.
        let mut admit_hash = None;
        if let Some(candidate) = handle
            .doc
            .read_queue()?
            .into_iter()
            .find(|item| item.id == id && item.revision == expected_revision)
            && matches!(candidate.status, QueueMessageStatus::Held)
            && candidate.delivery_gate.is_none()
        {
            let hash = executable_row_hash(&candidate)?;
            if !self.inner.store.local_executable_matches(
                chat_id,
                LocalExecutableKind::Queue,
                id,
                &hash,
            )? {
                let receipt = self.task_command_receipt(command_id)?.ok_or_else(|| {
                    EngineError::Other("explicit queue action has no local receipt".into())
                })?;
                let expected_intent = if send == QueueSend::Steer {
                    "steerQueuedNow"
                } else {
                    "sendQueuedNow"
                };
                if receipt.task_id != chat_id
                    || receipt.status != CommandReceiptStatus::Pending
                    || receipt.intent != expected_intent
                    || receipt.correlation_id.as_deref() != Some(id)
                {
                    return Err(EngineError::Other(
                        "explicit queue action lacks pending local authority".into(),
                    ));
                }
                admit_hash = Some(hash);
            }
        }
        let (claim, committed) = self.correlate_queue_task_projection(
            &handle,
            command_id,
            id,
            admit_hash.as_deref(),
            |doc| {
                if let Some(expected_hash) = admit_hash.as_deref() {
                    let current = doc.read_queue()?.into_iter().find(|row| row.id == id);
                    if current
                        .as_ref()
                        .map(executable_row_hash)
                        .transpose()?
                        .as_deref()
                        != Some(expected_hash)
                    {
                        return Err(EngineError::Other(
                            "queued row changed after local review".into(),
                        ));
                    }
                }
                let outcome =
                    doc.claim_queued(id, expected_revision, command_id, mode, now_ms())?;
                let commit = matches!(outcome, QueueClaimOutcome::Claimed(_));
                Ok((outcome, commit))
            },
        )?;
        let item = match claim {
            QueueClaimOutcome::Claimed(item) => {
                debug_assert!(committed.is_some());
                item
            }
            QueueClaimOutcome::AlreadyClaimed(item) => {
                return Ok(QueueDispatchOutcome::AlreadyClaimed(item));
            }
            QueueClaimOutcome::RevisionConflict(item) => {
                return Ok(QueueDispatchOutcome::RevisionConflict(item));
            }
            QueueClaimOutcome::Blocked(item) => {
                return Ok(QueueDispatchOutcome::Blocked(item));
            }
            QueueClaimOutcome::Missing => return Ok(QueueDispatchOutcome::Missing),
        };
        let was_paused = handle.queue_paused.swap(false, Ordering::AcqRel);
        let provider_outcome = match self.dispatch_queued(&handle, &item, send).await {
            Ok(outcome) => outcome,
            Err(error) => {
                if was_paused {
                    handle.queue_paused.store(true, Ordering::Release);
                }
                self.mark_queue_task_unknown(
                    &handle,
                    command_id,
                    "provider dispatch returned without a provable acceptance result",
                    |doc| {
                        doc.mark_queued_claim_unknown(id, command_id, now_ms())?;
                        Ok(())
                    },
                )?;
                return Err(error);
            }
        };
        // Send/Steer now selects exactly one row. If Stop, restart recovery,
        // or a prior ambiguous dispatch had frozen the queue, preserve that
        // hold after starting the selected row; the idle transition must not
        // immediately claim an unrelated sibling.
        if was_paused {
            handle.queue_paused.store(true, Ordering::Release);
        }
        match provider_outcome {
            QueueProviderOutcome::Accepted => {
                if send == QueueSend::Steer {
                    let result = self.persist_queue_projection_if(
                        &handle,
                        &format!("steer-accepted:{command_id}"),
                        |doc| {
                            let row = doc.mark_queued_steer_accepted(id, command_id, now_ms())?;
                            let changed = row.is_some();
                            Ok((row, changed))
                        },
                    );
                    if let Err(error) = result {
                        let _ = self.mark_queue_task_unknown(
                            &handle,
                            command_id,
                            "provider accepted native steering, but the durable acceptance projection failed",
                            |doc| {
                                doc.mark_queued_claim_unknown(id, command_id, now_ms())?;
                                Ok(())
                            },
                        );
                        return Err(error);
                    }
                }
                Ok(QueueDispatchOutcome::Started)
            }
            QueueProviderOutcome::Unknown => {
                let (row, receipt) = self.mark_queue_task_unknown(
                    &handle, command_id,
                    "native steer acceptance is unknown; the queued row will not be replayed automatically",
                    |doc| {
                        doc.mark_queued_claim_unknown(id, command_id, now_ms())?;
                        Ok(doc.read_queue()?.into_iter().find(|item| item.id == id))
                    },
                )?;
                match row {
                    Some(row) => Ok(QueueDispatchOutcome::Unknown(row)),
                    None if receipt.status == CommandReceiptStatus::Applied => {
                        Ok(QueueDispatchOutcome::Started)
                    }
                    None => Err(EngineError::Other(
                        "queued steer claim vanished without native confirmation".into(),
                    )),
                }
            }
            QueueProviderOutcome::Rejected(reason) => {
                let row = self.reject_queue_task(&handle, id, command_id, reason)?;
                Ok(QueueDispatchOutcome::Rejected(row))
            }
        }
    }

    /// Attempt native in-turn steering for a row whose persisted intent is
    /// Steer. It never falls back to interrupt or a fresh turn.
    pub async fn steer_queued_now(&self, chat_id: &str, id: &str) -> Result<bool, EngineError> {
        self.steer_queued_now_for_turn(chat_id, id, None).await
    }

    pub async fn steer_queued_now_for_turn(
        &self,
        chat_id: &str,
        id: &str,
        expected_turn_id: Option<&str>,
    ) -> Result<bool, EngineError> {
        if !self.is_host(chat_id) {
            return Err(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            )));
        }
        let Some(candidate) = self.queued_message(chat_id, id)? else {
            return Ok(false);
        };
        if candidate.delivery_gate.is_some() {
            return Err(EngineError::Other(
                "queued message is blocked for editing or review".into(),
            ));
        }
        let command_id = self.begin_legacy_queue_delivery(chat_id, &candidate, "steerQueuedNow")?;
        let outcome = self
            .steer_queued_now_with_claim(
                chat_id,
                id,
                candidate.revision,
                &command_id,
                expected_turn_id,
            )
            .await?;
        if matches!(
            outcome,
            QueueDispatchOutcome::Blocked(_)
                | QueueDispatchOutcome::RevisionConflict(_)
                | QueueDispatchOutcome::Missing
        ) {
            let active_turn_id = self
                .sessions()
                .and_then(|sessions| sessions.active_turn_id(chat_id));
            let _ = self.complete_task_command(
                &command_id,
                CommandReceiptStatus::Rejected,
                false,
                None,
                None,
                active_turn_id.as_deref(),
                Some(CommandReceiptReason {
                    code: CommandReasonCode::UnsupportedOperation,
                    message: "native in-turn steering is not currently safe for this row".into(),
                }),
                Some("The message remains queued; retry after the hold reason changes."),
            );
        }
        Ok(matches!(
            outcome,
            QueueDispatchOutcome::Started | QueueDispatchOutcome::AlreadyClaimed(_)
        ))
    }

    /// Host-only: hand queued messages to the agent when there is somewhere to
    /// put them.
    ///
    /// - Idle: claim and send the head as the next turn.
    /// - Turn in flight: hold. The turn-end watcher comes back for it.
    ///
    /// One at a time by design: each send changes the status this reads.
    pub async fn drain_queue(&self, handle: &Arc<ChatDocHandle>) {
        let Some(sessions) = self.sessions() else {
            return; // executor not wired yet; the set_sessions kick re-drains
        };
        if !self.is_host(&handle.chat_id) {
            return;
        }
        // One drain at a time per chat. Waiters are cheap: whoever takes the
        // lock next re-reads the queue and the status, so a drain that became
        // unnecessary while it waited simply finds nothing to do.
        let _drain = handle.drain_lock.lock().await;
        if handle.queue_paused.load(Ordering::Acquire) {
            return;
        }
        if handle.doc.context_control().pending_rewind.is_some() {
            return;
        }
        if handle.doc.context_control().compact.blocks_new_turn() {
            return;
        }
        let Ok(Some(head)) = handle.doc.read_queue().map(|q| q.into_iter().next()) else {
            return;
        };
        let hash = match executable_row_hash(&head) {
            Ok(hash) => hash,
            Err(error) => {
                tracing::warn!(chat = %handle.chat_id, row = %head.id, %error,
                    "queued-row authority hash unavailable; stopping drain");
                return;
            }
        };
        let authorized = match self.inner.store.local_executable_matches(
            &handle.chat_id,
            LocalExecutableKind::Queue,
            &head.id,
            &hash,
        ) {
            Ok(authorized) => authorized,
            Err(error) => {
                tracing::warn!(chat = %handle.chat_id, row = %head.id, %error,
                    "queued-row authority unavailable; stopping drain");
                return;
            }
        };
        if !authorized {
            tracing::warn!(chat = %handle.chat_id, row = %head.id,
                "untrusted synced queued row quarantined before provider dispatch");
            if matches!(head.status, QueueMessageStatus::Held)
                && head.delivery_gate.is_none()
                && head.hold_reason != Some(QueueHoldReason::RemoteAuthorityRequired)
            {
                let _ = self.persist_queue_projection_if(
                    handle,
                    &format!("remote-review:{}", head.id),
                    |doc| {
                        let changed = doc
                            .set_queued_hold_reason(
                                &head.id,
                                QueueHoldReason::RemoteAuthorityRequired,
                            )?
                            .is_some();
                        Ok((changed, changed))
                    },
                );
            }
            return;
        }
        match &head.delivery_gate {
            Some(QueueDeliveryGate::Editing {
                lease_id,
                owner_device_id,
                expires_at_ms,
                base_text_hash,
                ..
            }) if *expires_at_ms <= now_ms() => {
                let review = QueueDeliveryGate::ReviewRequired {
                    previous_lease_id: lease_id.clone(),
                    owner_device_id: owner_device_id.clone(),
                    since_ms: now_ms(),
                    base_text_hash: base_text_hash.clone(),
                };
                let _ = self.persist_queue_projection_if(
                    handle,
                    &format!("edit-review:{}", uuid::Uuid::new_v4()),
                    |doc| {
                        let changed = doc.set_queued_delivery_gate(&head.id, Some(&review))?;
                        Ok((changed, changed))
                    },
                );
                return;
            }
            Some(QueueDeliveryGate::Editing {
                lease_id,
                expires_at_ms,
                ..
            }) => {
                self.arm_queue_edit_expiry(handle, &head.id, lease_id, *expires_at_ms);
                return;
            }
            Some(QueueDeliveryGate::ReviewRequired { .. }) => return,
            None => {}
        }
        // In flight, not just Working: an agent parked on a question owns
        // the turn too, and the composer queues on the same reading. Taking
        // `AwaitingInput` for idle would send the follow-up as a fresh turn
        // and abandon the question.
        if sessions.turn_in_flight(&handle.chat_id) {
            return; // All queued messages wait, including rows from older clients.
        }
        if self.request_from_chat_row(&handle.chat_id, "").is_none()
            && sessions.last_request(&handle.chat_id).is_none()
        {
            handle.queue_paused.store(true, Ordering::Release);
            return;
        }
        if !matches!(head.status, QueueMessageStatus::Held) {
            // Claimed/unknown work survives restart visibly and is never
            // replayed merely because the provider is currently idle.
            return;
        }
        if head.follow_up_behavior != luna_proto::FollowUpBehavior::Queue {
            // Follow-up intent is immutable. A rejected or otherwise blocked
            // Steer remains the FIFO head until the user explicitly retries,
            // removes, or sends it; edits preserve that intent. Terminal Done
            // and later document notifications must never downgrade it into
            // a fresh turn.
            return;
        }
        let send = QueueSend::NextTurn;
        let command_id = format!(
            "queue-dispatch:{}:{}:{}",
            handle.chat_id, head.id, head.revision
        );
        let now = now_ms();
        let snapshot = match self.task_command_snapshot(&handle.chat_id) {
            Ok(read) => read.snapshot,
            Err(error) => {
                tracing::warn!(chat = %handle.chat_id, %error, "queue receipt snapshot failed");
                return;
            }
        };
        let pending = CommandReceipt {
            command_id: command_id.clone(),
            task_id: handle.chat_id.clone(),
            intent: "dispatchQueued".into(),
            payload_hash: format!(
                "{:x}",
                Sha256::digest(format!("{}:{}:{}", head.id, head.revision, head.text).as_bytes())
            ),
            status: CommandReceiptStatus::Pending,
            snapshot: snapshot.clone(),
            event_sequence: None,
            actor_id: self.inner.config.device_id.clone(),
            client_id: "queue-drain".into(),
            client_session_id: "engine".into(),
            execution_environment: Some(self.inner.config.device_id.clone()),
            correlation_id: None,
            reason: None,
            recovery_action: None,
            accepted_at_ms: now,
            committed_at_ms: None,
            observed_at_ms: now,
        };
        let begin = match self.begin_task_command(pending, None, None, None) {
            Ok(begin) => begin,
            Err(error) => {
                tracing::warn!(chat = %handle.chat_id, %error, "queue receipt acceptance failed");
                return;
            }
        };
        match begin.kind {
            luna_sync::TaskCommandBeginKind::Accepted => {}
            luna_sync::TaskCommandBeginKind::Duplicate
                if begin.receipt.status == CommandReceiptStatus::Applied =>
            {
                let _ = self.persist_queue_projection_if(
                    handle,
                    &format!("settle:{command_id}"),
                    |doc| {
                        let changed = doc.settle_queued_claim(&head.id, &command_id)?;
                        Ok((changed, changed))
                    },
                );
                return;
            }
            luna_sync::TaskCommandBeginKind::Duplicate => {
                let _ = self.mark_queue_task_unknown(
                    handle,
                    &command_id,
                    "engine restarted after queue claim; reconcile provider state before retry",
                    |doc| {
                        let _ = doc.claim_queued(
                            &head.id,
                            head.revision,
                            &command_id,
                            QueueClaimMode::NextTurn,
                            now,
                        )?;
                        doc.mark_queued_claim_unknown(&head.id, &command_id, now)?;
                        Ok(())
                    },
                );
                return;
            }
            luna_sync::TaskCommandBeginKind::Conflict => return,
        }
        self.publish_task_receipt(begin.receipt);
        let (claim, _) =
            match self.correlate_queue_task_projection(handle, &command_id, &head.id, None, |doc| {
                let outcome = doc.claim_queued(
                    &head.id,
                    head.revision,
                    &command_id,
                    QueueClaimMode::NextTurn,
                    now,
                )?;
                let commit = matches!(outcome, QueueClaimOutcome::Claimed(_));
                Ok((outcome, commit))
            }) {
                Ok(result) => result,
                Err(error) => {
                    tracing::warn!(chat = %handle.chat_id, %error, "durable queue claim failed");
                    return;
                }
            };
        let item = match claim {
            QueueClaimOutcome::Claimed(item) => item,
            _ => {
                let _ = self.complete_task_command(
                    &command_id,
                    CommandReceiptStatus::Conflict,
                    false,
                    snapshot.binding_id.as_deref(),
                    snapshot.binding_generation,
                    snapshot.active_turn_id.as_deref(),
                    Some(CommandReceiptReason {
                        code: CommandReasonCode::OperationRejected,
                        message: "queued message changed before dispatch claim".into(),
                    }),
                    Some("Reload the durable queue before choosing another action."),
                );
                return;
            }
        };
        if let Err(err) = self.dispatch_queued(handle, &item, send).await {
            tracing::warn!(chat = %handle.chat_id, error = %err, "queued send failed");
            handle.queue_paused.store(true, Ordering::Release);
            let _ = self.mark_queue_task_unknown(
                handle,
                &command_id,
                "provider dispatch returned without a provable acceptance result",
                |doc| {
                    doc.mark_queued_claim_unknown(&head.id, &command_id, now_ms())?;
                    Ok(())
                },
            );
            return;
        }
        // The matching SessionStarted/Steered event removes this exact
        // claimed row and finalizes the pending receipt. Until then no
        // second queue row may drain behind an unconfirmed provider call.
    }

    /// Stop the active turn without treating the resulting Idle transition as
    /// permission to release the next queued message. The same lock used by
    /// drains closes the race between clicking Cancel and the status watcher.
    async fn interrupt_and_pause_queue(
        &self,
        sessions: &SessionsEngine,
        handle: &Arc<ChatDocHandle>,
    ) -> Result<bool, EngineError> {
        let _drain = handle.drain_lock.lock().await;
        if !sessions.turn_in_flight(&handle.chat_id) {
            return Ok(false);
        }
        handle.queue_paused.store(true, Ordering::Release);
        match sessions.interrupt_for_stop(&handle.chat_id).await {
            Ok(true) => Ok(true),
            Ok(false) => {
                handle.queue_paused.store(false, Ordering::Release);
                Ok(false)
            }
            Err(err) => {
                handle.queue_paused.store(false, Ordering::Release);
                Err(err)
            }
        }
    }

    /// Send one taken queue row.
    async fn dispatch_queued(
        &self,
        handle: &Arc<ChatDocHandle>,
        item: &QueuedMessage,
        send: QueueSend,
    ) -> Result<QueueProviderOutcome, EngineError> {
        let original_w = self
            .inner
            .store
            .local_executable_authority(&handle.chat_id, LocalExecutableKind::Queue, &item.id)?
            .ok_or_else(|| {
                EngineError::Other("queued row has no original workspace authority".into())
            })?
            .workspace_repair_revision;
        let hash = executable_row_hash(item)?;
        if !self.inner.store.local_executable_matches(
            &handle.chat_id,
            LocalExecutableKind::Queue,
            &item.id,
            &hash,
        )? {
            return Err(EngineError::Other(
                "queued row lacks exact local authority for provider dispatch".into(),
            ));
        }
        let Some(sessions) = self.sessions() else {
            return Err(EngineError::Other("sessions engine not wired".into()));
        };
        let chat_id = &handle.chat_id;
        // Keep the queue row's identity when it becomes a real user message.
        // The submitting viewport learns this id from QueueMessage and can
        // therefore wait without disturbing the active turn's runway, then
        // anchor the prompt only once this exact row reaches the transcript.
        let message_id = item.id.clone();
        let mut prompt = queued_message_prompt(&item.text, &item.attachments);
        let mut attachments = item.attachments.clone();
        self.resolve_input_attachments(&mut prompt, &mut attachments);
        if send == QueueSend::Steer {
            match sessions
                .steer_with_inputs_at_workspace_revision(
                    chat_id,
                    &prompt,
                    &attachments,
                    &item.input_items,
                    Some(message_id.clone()),
                    original_w,
                )
                .await?
            {
                SteerOutcome::Accepted => return Ok(QueueProviderOutcome::Accepted),
                SteerOutcome::Rejected => {
                    return Ok(QueueProviderOutcome::Rejected(
                        QueueHoldReason::ProviderRejected,
                    ));
                }
                SteerOutcome::NotSteerable | SteerOutcome::Busy => {
                    return Ok(QueueProviderOutcome::Rejected(
                        QueueHoldReason::SteeringUnavailable,
                    ));
                }
                SteerOutcome::Unknown => return Ok(QueueProviderOutcome::Unknown),
            }
        }
        // Same reading of "busy" as the drain: a turn parked on a question is
        // still a turn, and it has to be stopped before this one starts.
        if send == QueueSend::Interrupt && sessions.turn_in_flight(chat_id) {
            sessions.interrupt(chat_id).await?;
        }
        let previous = sessions.last_request(chat_id);
        let request = self
            .request_from_chat_row(chat_id, &prompt)
            .map(|mut current| {
                if let Some(previous) = &previous {
                    current.auto_approve = previous.auto_approve;
                    current.worktree = previous.worktree.clone();
                }
                current
            })
            .or(previous);
        let Some(mut request) = request else {
            return Err(EngineError::Other(
                "no live run and no prior run config".into(),
            ));
        };
        if let Some(execution) = &item.execution {
            request.harness = Some(execution.harness);
            request.provider_account_id = execution.provider_account_id.clone();
            request.model = execution.model.clone();
            request.reasoning = execution.reasoning;
            request.model_options = execution.model_options.clone();
            request.sandbox = execution.sandbox;
            request.permission_mode = execution.permission_mode;
        }
        request.review = None;
        request.prompt = prompt;
        request.resume = None; // dispatch re-derives the harness session
        request.attachments = attachments;
        request.input_items = item.input_items.clone();
        let harness = self.harness_for_request(chat_id, &request);
        self.dispatch_with_source_context(
            &sessions,
            chat_id,
            harness,
            request,
            Some(message_id),
            original_w,
        )
        .await?;
        Ok(QueueProviderOutcome::Accepted)
    }

    /// POST `{edge}/device/{host}/nudge {chatId}` when the chat's workspace row names
    /// another device as host. Best-effort: offline/edge-less engines skip silently.
    fn nudge_remote_host(&self, chat_id: &str) {
        if self.is_side_document(chat_id) {
            return;
        }
        let Some(edge) = self.inner.config.edge.clone() else {
            return;
        };
        let Some(workspace) = self.workspace() else {
            return;
        };
        let host_device = match workspace.chat(chat_id) {
            Ok(Some(chat)) => chat.device_id,
            // Unclaimed chat: whoever drains first claims it — nobody to nudge.
            _ => return,
        };
        if host_device == self.inner.config.device_id {
            return;
        }
        // Only meaningful inside a runtime (RPC handlers, executors); bare sync
        // callers (unit tests) skip rather than panic.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let url = format!(
            "{}/device/{}/nudge",
            edge.url.trim_end_matches('/'),
            host_device
        );
        let chat = chat_id.to_string();
        self.spawn_worker_on(&runtime, async move {
            // Fresh bearer per request — never the boot-time snapshot.
            let bearer = match edge.bearer().await {
                Ok(bearer) => bearer,
                Err(err) => {
                    tracing::warn!(chat = %chat, error = %err, "nudge skipped: token unavailable");
                    return;
                }
            };
            let send = reqwest::Client::new()
                .post(&url)
                .bearer_auth(&bearer)
                .json(&serde_json::json!({ "chatId": chat }))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await;
            match send {
                Ok(res) if res.status().is_success() => {
                    tracing::info!(chat = %chat, device = %host_device, "host nudged");
                }
                Ok(res) => tracing::warn!(chat = %chat, device = %host_device,
                    status = res.status().as_u16(), "nudge rejected"),
                Err(err) => {
                    let err = describe_http_error(err);
                    tracing::warn!(chat = %chat, error = %err, "nudge failed (best-effort)")
                }
            }
        });
    }

    /// The chat's host device when it is NOT this engine (mirrors
    /// `nudge_remote_host`'s ownership read).
    fn remote_host_for(&self, chat_id: &str) -> Option<String> {
        let workspace = self.workspace()?;
        let host_device = match workspace.chat(chat_id) {
            Ok(Some(chat)) => chat.device_id,
            _ => return None,
        };
        (host_device != self.inner.config.device_id).then_some(host_device)
    }

    /// Durable-delivery escort for one queued command aimed at a REMOTE host:
    ///
    /// 1. push any queued attachment bytes over the peer link (retry until
    ///    they land — the relayed command must never outrun its bytes);
    /// 2. give the normal path (chat2 rows → edge → host's room) a short
    ///    grace to ack;
    /// 3. rows still not at the edge but the peer link alive → relay-forward
    ///    the entry itself ([`luna_rpc::methods::RELAY_COMMAND`]). The
    ///    host's processed ledger claims the client-minted id, so the doc
    ///    row arriving later dedupes to a no-op — exactly-once by
    ///    construction (the 2026-08-18 03:45 incident shape: nudges flowed,
    ///    rows didn't; there was no second road for the command).
    ///
    /// Stops the moment any path lands. No-op for locally-hosted chats.
    fn spawn_command_delivery(
        &self,
        chat_id: &str,
        entry: SessionCommandEntry,
        transfers: Vec<crate::uploads::AttachmentTransfer>,
    ) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return; // bare sync callers (unit tests) skip rather than panic
        };
        let host = self.clone();
        let chat = chat_id.to_string();
        self.spawn_worker_on(&runtime, async move {
            let Some(target) = host.remote_host_for(&chat) else {
                return; // local host (or no row yet claimed remotely)
            };
            if !transfers.is_empty() && !host.deliver_attachments(&chat, &transfers).await {
                return; // gave up; the drain's wait cap surfaces the failure
            }
            let mut wake = luna_sync::wake::subscribe();
            let mut online = luna_sync::wake::subscribe_online();
            let give_up = tokio::time::Instant::now() + RELAY_GIVE_UP;
            let grace_end = tokio::time::Instant::now() + ROWS_GRACE;
            while tokio::time::Instant::now() < grace_end {
                if host.rows_flushed(&chat) {
                    return; // rows on the edge — the normal path has it
                }
                tokio::time::sleep(ROWS_POLL).await;
            }
            let mut backoff = RELAY_BACKOFF_BASE;
            loop {
                if host.rows_flushed(&chat) {
                    return; // the normal path won while we were retrying
                }
                match host.relay_command(&target, &chat, &entry).await {
                    Ok(outcome) => {
                        tracing::info!(chat = %chat, device = %target, command = %entry.id,
                            outcome, "command delivered via peer relay");
                        return;
                    }
                    Err(err) => {
                        tracing::warn!(chat = %chat, device = %target, error = %err,
                            backoff_ms = backoff.as_millis() as u64,
                            "peer-relay delivery retrying");
                    }
                }
                if tokio::time::Instant::now() >= give_up {
                    tracing::warn!(chat = %chat, command = %entry.id,
                        "peer-relay delivery gave up; command remains queued in the doc");
                    return;
                }
                while wake.try_recv().is_ok() {}
                while online.try_recv().is_ok() {}
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = wake.recv() => {}
                    _ = online.recv() => {}
                }
                backoff = (backoff * 2).min(RELAY_BACKOFF_CAP);
            }
        });
    }

    /// Step 1 of the escort: push staged bytes until they land (event-driven
    /// backoff), `true` on success. Re-resolves the host each attempt (a
    /// claim can move the chat).
    async fn deliver_attachments(
        &self,
        chat: &str,
        transfers: &[crate::uploads::AttachmentTransfer],
    ) -> bool {
        let mut wake = luna_sync::wake::subscribe();
        let mut online = luna_sync::wake::subscribe_online();
        let mut backoff = TRANSFER_BACKOFF_BASE;
        let deadline = tokio::time::Instant::now() + ATTACHMENT_WAIT_MAX;
        loop {
            let Some(target) = self.remote_host_for(chat) else {
                return true; // became locally hosted: bytes already here
            };
            match self.push_attachments(&target, transfers).await {
                Ok(()) => {
                    tracing::info!(chat = %chat, device = %target,
                        count = transfers.len(), "queued attachments delivered");
                    // The bytes beat the drain's next look — kick it via the
                    // durable nudge (the host's UploadCommit already kicked
                    // its local drains too).
                    self.nudge_remote_host(chat);
                    return true;
                }
                Err(TransferError::Permanent(err)) => {
                    tracing::warn!(chat = %chat, device = %target, error = %err,
                        "queued attachment transfer failed permanently");
                    return false;
                }
                Err(TransferError::Transient(err)) => {
                    tracing::warn!(chat = %chat, device = %target, error = %err,
                        backoff_ms = backoff.as_millis() as u64,
                        "queued attachment transfer retrying");
                }
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(chat = %chat, "queued attachment transfer gave up (wait cap)");
                return false;
            }
            while wake.try_recv().is_ok() {}
            while online.try_recv().is_ok() {}
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = wake.recv() => {}
                _ = online.recv() => {}
            }
            backoff = (backoff * 2).min(TRANSFER_BACKOFF_CAP);
        }
    }

    /// Every local chat2 batch acked while connected — our rows are ON the
    /// edge, and the host's own room connection will deliver them. (A room
    /// that hasn't joined keeps pre-join updates in a local buffer, so
    /// `connected` is load-bearing here, not just the empty queue.)
    fn rows_flushed(&self, chat_id: &str) -> bool {
        let handle = lock(&self.inner.handles).get(chat_id).cloned();
        handle
            .and_then(|h| lock(&h.chat2).as_ref().map(|c| c.stats()))
            .is_some_and(|s| s.connected && s.pending_pushes == 0)
    }

    /// One relay attempt: version-gate the host, then forward the entry over
    /// the peer link. Timeouts mark the link suspect (drop + redial next
    /// attempt); a host-side refusal is permanent for THIS attempt but the
    /// escort keeps retrying until the give-up cap (the refusal may be
    /// "attachments not landed yet").
    async fn relay_command(
        &self,
        target: &str,
        chat_id: &str,
        entry: &SessionCommandEntry,
    ) -> Result<&'static str, String> {
        let supported = self
            .workspace()
            .and_then(|ws| ws.read_devices().ok())
            .into_iter()
            .flatten()
            .find(|d| d.id == target)
            .and_then(|d| d.version.as_deref().and_then(luna_proto::version_triple))
            .is_some_and(|v| v >= RELAY_MIN_VERSION);
        if !supported {
            return Err("host does not support relay delivery (version gate)".into());
        }
        let links = self
            .inner
            .links
            .get()
            .ok_or_else(|| "peer links not wired".to_string())?;
        let client = links
            .client(target)
            .await
            .map_err(|e| format!("peer link: {e}"))?;
        let params = serde_json::json!({ "chatId": chat_id, "entry": entry });
        let call = client.call(luna_rpc::methods::RELAY_COMMAND, params);
        match tokio::time::timeout(RELAY_CALL_TIMEOUT, call).await {
            Err(_) => {
                links.invalidate(target);
                Err("relay call timed out; peer link suspect".into())
            }
            Ok(Err(luna_rpc::RpcError::Failed(err))) => Err(format!("host refused: {err}")),
            Ok(Err(err)) => {
                links.invalidate(target);
                Err(format!("relay call failed: {err}"))
            }
            Ok(Ok(reply)) => Ok(match reply.get("outcome").and_then(|v| v.as_str()) {
                Some("executed") => "executed",
                Some("duplicate") => "duplicate",
                Some("expired") => "expired",
                Some("superseded") => "superseded",
                _ => "accepted",
            }),
        }
    }

    /// User-driven retry (the failed-send affordance): re-kick every
    /// delivery road for a chat whose queued sends haven't been adopted —
    /// fresh chat2 socket (a zombie room is the usual suspect), host nudge,
    /// a local drain pass, and a fresh delivery escort per still-pending
    /// command with its attachment transfers re-derived from the entries'
    /// `pending://` refs (idempotent: re-pushing landed bytes re-commits the
    /// same file; the processed ledger keeps execution exactly-once).
    pub fn retry_delivery(&self, chat_id: &str) -> Result<(), EngineError> {
        let handle = self.open(chat_id)?;
        if let Some(chat2) = lock(&handle.chat2).as_ref() {
            chat2.redial();
        }
        self.nudge_remote_host(chat_id);
        let commands = handle.doc.read_commands()?;
        let pending: Vec<SessionCommandEntry> = commands
            .iter()
            .filter(|c| {
                c.status == SessionCommandStatus::Pending
                    && !self.inner.store.is_processed(&c.id).unwrap_or(false)
            })
            .cloned()
            .collect();
        for entry in pending {
            let transfers = command_transfers(&entry);
            self.spawn_command_delivery(chat_id, entry, transfers);
        }
        // Dead attempts: a Run/Steer whose user message never landed and
        // whose command can never execute again — Rejected (execute failed,
        // or the dead-command sweep terminalized it), or consumed by the
        // ledger with no outcome and not currently executing (crash between
        // mark and resolve). Exactly-once is per command ID, so a
        // user-driven retry mints a FRESH attempt: new id, same payload and
        // message id (the executor's user-entry pre-write dedupes by message
        // id). One re-issue per message — the LATEST attempt speaks for it.
        let messages = handle.doc.read_entries().unwrap_or_default();
        let message_landed = |mid: &str| messages.iter().any(|m| m.id == mid);
        let mut latest_dead: HashMap<String, &SessionCommandEntry> = HashMap::new();
        for c in &commands {
            let Some(mid) = (match &c.payload {
                SessionCommandPayload::Run { message_id, .. } => Some(message_id.as_str()),
                SessionCommandPayload::Steer { message_id, .. } => message_id.as_deref(),
                _ => None,
            }) else {
                continue;
            };
            if message_landed(mid) {
                continue;
            }
            let dead = match c.status {
                // Rejected: execute failed or the sweep terminalized a crash
                // window. Expired: the entry outlived its TTL undelivered —
                // an explicit user retry is exactly the consent to re-send.
                SessionCommandStatus::Rejected | SessionCommandStatus::Expired => true,
                SessionCommandStatus::Pending => {
                    self.inner.store.is_processed(&c.id).unwrap_or(false)
                        && !lock(&self.inner.executing).contains_key(&c.id)
                }
                _ => false,
            };
            if !dead {
                continue;
            }
            // A LIVE pending attempt for the same message (queued or being
            // escorted above) makes a re-issue a duplicate — skip.
            let live_attempt = commands.iter().any(|o| {
                o.id != c.id
                    && o.status == SessionCommandStatus::Pending
                    && !self.inner.store.is_processed(&o.id).unwrap_or(false)
                    && match (&o.payload, &c.payload) {
                        (
                            SessionCommandPayload::Run { message_id: a, .. },
                            SessionCommandPayload::Run { message_id: b, .. },
                        ) => a == b,
                        (
                            SessionCommandPayload::Steer { message_id: a, .. },
                            SessionCommandPayload::Steer { message_id: b, .. },
                        ) => a == b,
                        _ => false,
                    }
            });
            if live_attempt {
                continue;
            }
            match latest_dead.entry(mid.to_string()) {
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    if c.issued_at > slot.get().issued_at {
                        slot.insert(c);
                    }
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(c);
                }
            }
        }
        for old in latest_dead.values() {
            if old.status == SessionCommandStatus::Pending {
                // Terminalize the consumed-but-dead original so the doc tells
                // the truth and the next retry pass doesn't see it again.
                self.resolve_command(
                    &handle,
                    &old.id,
                    SessionCommandStatus::Rejected,
                    Some("interrupted before completion — superseded by retry"),
                );
            }
            let now = now_ms();
            let reissue = SessionCommandEntry {
                id: new_id(),
                payload: old.payload.clone(),
                issued_by: self.inner.config.device_id.clone(),
                issued_at: now,
                based_on: messages.last().map(|m| CommandBasedOn {
                    turn_id: Some(m.id.clone()),
                    frontier: None,
                }),
                expires_at: Some(now + COMMAND_DEFAULT_TTL_MS),
                status: SessionCommandStatus::Pending,
                resolution: None,
            };
            tracing::info!(chat = %chat_id, old = %old.id, new = %reissue.id,
                "retry re-issues a dead send attempt");
            if !self.inner.store.record_local_executable(
                chat_id,
                LocalExecutableKind::Command,
                &reissue.id,
                &executable_row_hash(&reissue)?,
                None,
            )? {
                return Err(EngineError::Other(
                    "retry command id already has different local authority".into(),
                ));
            }
            handle.doc.queue_command(&reissue)?;
            let transfers = command_transfers(&reissue);
            self.spawn_command_delivery(chat_id, reissue, transfers);
        }
        // Locally-hosted (or already-synced) commands: a drain pass is the
        // whole retry.
        if tokio::runtime::Handle::try_current().is_ok() {
            let host = self.clone();
            let handle = handle.clone();
            self.spawn_worker(async move { host.drain_commands(&handle).await });
        }
        Ok(())
    }

    /// Host side of [`luna_rpc::methods::RELAY_COMMAND`]: evaluate the
    /// forwarded entry against OUR doc (dedupe/TTL/supersede rules apply
    /// unchanged), claim its client-minted id in the processed ledger, then
    /// execute. The claim is what makes the doc row arriving later — over
    /// chat2 sync — a no-op in the drain: exactly-once across both roads.
    pub async fn ingest_relayed_command(
        &self,
        chat_id: &str,
        entry: SessionCommandEntry,
    ) -> Result<&'static str, EngineError> {
        if !self.inner.store.local_executable_matches(
            chat_id,
            LocalExecutableKind::Command,
            &entry.id,
            &executable_row_hash(&entry)?,
        )? {
            return Err(EngineError::Other(
                "relayed command lacks exact local authority".into(),
            ));
        }
        let handle = self.open(chat_id)?;
        // The sender sequences attachment transfers BEFORE the relay; refuse
        // (retryably) rather than run without the images.
        if !self.missing_attachments(&entry).is_empty() {
            return Err(EngineError::Other("attachments not landed yet".into()));
        }
        let sessions = self
            .sessions()
            .ok_or_else(|| EngineError::Other("executor unavailable".into()))?;
        let commands = handle.doc.read_commands()?;
        let messages = handle.doc.read_entries().unwrap_or_default();
        let current_turn_id = messages.last().map(|m| m.id.clone());
        let turn_is_past = |turn_id: &str| messages.iter().any(|m| m.id == turn_id);
        let is_processed = |id: &str| self.inner.store.is_processed(id).unwrap_or(false);
        let disposition = evaluate_command(
            &entry,
            &EvaluationContext {
                is_processed: &is_processed,
                now_ms: now_ms(),
                entries: &commands,
                current_turn_id: current_turn_id.as_deref(),
                turn_is_past: &turn_is_past,
            },
        );
        if matches!(disposition, CommandDisposition::Skip) {
            return Ok("duplicate");
        }
        // In-flight claim first (the drain's dead-command sweep must see this
        // id as alive, not crashed, while the execute below runs).
        if !self.claim_executing(chat_id, &entry.id) {
            return Ok("duplicate");
        }
        // Claim BEFORE executing (the drain's own mark-before-execute rule).
        let marked = self.inner.store.mark_processed(&entry.id);
        let result = match marked {
            Err(err) => Err(err.into()),
            Ok(false) => Ok("duplicate"),
            Ok(true) => match disposition {
                CommandDisposition::Skip => Ok("duplicate"),
                CommandDisposition::Expired => Ok("expired"),
                CommandDisposition::Superseded => Ok("superseded"),
                CommandDisposition::Execute => match self.execute(&sessions, &handle, &entry).await
                {
                    Ok(_) => Ok("executed"),
                    Err(err) => Err(err),
                },
            },
        };
        lock(&self.inner.executing).remove(&entry.id);
        result
    }

    /// One transfer attempt: chunked `UploadChunk` + `UploadCommit` straight
    /// over the peer link (same wire the UI's legacy path used, so old and
    /// new engines interoperate). Timeouts mark the link suspect —
    /// `invalidate` drops the cached socket so the retry dials fresh instead
    /// of feeding a zombie pipe forever (2026-08-19 incident).
    async fn push_attachments(
        &self,
        target: &str,
        transfers: &[crate::uploads::AttachmentTransfer],
    ) -> Result<(), TransferError> {
        use TransferError::{Permanent, Transient};
        let Some(links) = self.inner.links.get() else {
            return Err(Permanent("peer links not wired".into()));
        };
        let Some(uploads) = self.inner.uploads.get() else {
            return Err(Permanent("uploads not wired".into()));
        };
        let client = links
            .client(target)
            .await
            .map_err(|e| Transient(format!("peer link: {e}")))?;
        for transfer in transfers {
            // Bytes come from the uploads jail only — a transfer names an
            // upload identity, never an arbitrary path.
            let source = uploads.pending_target(&transfer.upload_id, &transfer.file_name);
            let bytes = tokio::fs::read(&source)
                .await
                .map_err(|e| Permanent(format!("staged attachment missing: {e}")))?;
            // Progress entry for the sender's thumbnail ring, updated per
            // landed chunk. The guard retires it on EVERY exit — commit,
            // timeout, refusal — so a dead attempt falls back to the
            // indeterminate spinner and the retry re-publishes from 0.
            let total = bytes.len() as u64;
            self.transfer_progress_set(&transfer.upload_id, &transfer.file_name, 0, total);
            let _progress = TransferProgressGuard {
                host: self,
                upload_id: &transfer.upload_id,
            };
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let mut start = 0usize;
            let mut seq = 0u64;
            loop {
                let end = (start + TRANSFER_CHUNK_B64).min(b64.len());
                let params = serde_json::json!({
                    "uploadId": transfer.upload_id, "seq": seq, "data": &b64[start..end],
                });
                let call = client.call(luna_rpc::methods::UPLOAD_CHUNK, params);
                match tokio::time::timeout(TRANSFER_CHUNK_TIMEOUT, call).await {
                    Err(_) => {
                        links.invalidate(target);
                        return Err(Transient("chunk push timed out; peer link suspect".into()));
                    }
                    Ok(Err(luna_rpc::RpcError::Failed(err))) => {
                        return Err(Permanent(format!("host refused chunk: {err}")));
                    }
                    Ok(Err(err)) => {
                        links.invalidate(target);
                        return Err(Transient(format!("chunk push failed: {err}")));
                    }
                    Ok(Ok(_)) => {}
                }
                start = end;
                seq += 1;
                // b64 → raw: 4 chars carry 3 bytes; min-clamp absorbs the
                // final chunk's padding overshoot.
                let done = ((start as u64) * 3 / 4).min(total);
                self.transfer_progress_set(&transfer.upload_id, &transfer.file_name, done, total);
                if start >= b64.len() {
                    break;
                }
            }
            let params = serde_json::json!({
                "uploadId": transfer.upload_id, "fileName": transfer.file_name,
            });
            let call = client.call(luna_rpc::methods::UPLOAD_COMMIT, params);
            match tokio::time::timeout(TRANSFER_COMMIT_TIMEOUT, call).await {
                Err(_) => {
                    links.invalidate(target);
                    return Err(Transient("commit timed out; peer link suspect".into()));
                }
                Ok(Err(luna_rpc::RpcError::Failed(err))) => {
                    return Err(Permanent(format!("host refused commit: {err}")));
                }
                Ok(Err(err)) => {
                    links.invalidate(target);
                    return Err(Transient(format!("commit failed: {err}")));
                }
                Ok(Ok(_)) => {}
            }
        }
        Ok(())
    }

    /// Upload a tool result's full output/diff to the R2 sidecar
    /// (`PUT {edge}/blob/{chatId}/{partId}[.diff]`, docs/chat2-sync.md A2).
    /// Fire-and-forget: the doc already carries the summary, so a lost upload
    /// degrades to "full output unavailable" — it must never block or fail
    /// the run. Offline/edge-less engines skip silently.
    pub fn upload_tool_sidecar(&self, chat_id: &str, payload: luna_doc::SidecarPayload) {
        if self.is_side_document(chat_id) {
            return;
        }
        let Some(edge) = self.inner.config.edge.clone() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return; // bare sync callers (unit tests) skip rather than panic
        };
        let http = self.inner.http.clone();
        let base = format!(
            "{}/blob/{}/{}",
            edge.url.trim_end_matches('/'),
            chat_id,
            encode_part_segment(&payload.part_id)
        );
        self.spawn_worker_on(&runtime, async move {
            let Ok(bearer) = edge.bearer().await else {
                return; // token unavailable; serve the local summary
            };
            let mut puts: Vec<(String, &'static str, Vec<u8>)> = Vec::new();
            if let Some(output) = &payload.output {
                puts.push((
                    base.clone(),
                    "text/plain; charset=utf-8",
                    output.clone().into_bytes(),
                ));
            }
            if let Some(diff) = &payload.diff
                && let Ok(json) = serde_json::to_vec(diff)
            {
                puts.push((format!("{base}.diff"), "application/json", json));
            }
            for (url, content_type, body) in puts {
                let sent = http
                    .put(&url)
                    .bearer_auth(&bearer)
                    .header("content-type", content_type)
                    .body(body)
                    .send()
                    .await;
                match sent {
                    Ok(res) if res.status().is_success() => {}
                    Ok(res) => tracing::warn!(url, status = res.status().as_u16(),
                        "tool sidecar upload rejected"),
                    Err(err) => {
                        let err = describe_http_error(err);
                        tracing::warn!(url, error = %err, "tool sidecar upload failed (best-effort)")
                    }
                }
            }
        });
    }

    /// Fetch a sidecar blob by its doc-resident ref (`{chatId}/{partId}` or
    /// `…​.diff`) — the UI's lazy "Show full output" path, served over RPC
    /// because the UI crate has no HTTP client or edge bearer.
    pub async fn fetch_tool_blob(&self, blob_ref: &str) -> Result<String, EngineError> {
        // Same shape `apply_sidecar_refs` writes; anything else is a forged ref.
        let valid = blob_ref.split_once('/').is_some_and(|(chat, part)| {
            !chat.is_empty()
                && chat.len() <= 128
                && chat
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                && !part.is_empty()
                && part.len() <= 200
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:#~-".contains(&b))
        });
        if !valid {
            return Err(EngineError::Other(format!("bad blob ref: {blob_ref}")));
        }
        if self.is_side_document(blob_ref.split_once('/').expect("validated above").0) {
            return Err(EngineError::Other(
                "This side-chat output is not available remotely".into(),
            ));
        }
        let Some(edge) = self.inner.config.edge.clone() else {
            return Err(EngineError::Other("offline: no edge configured".into()));
        };
        let bearer = edge.bearer().await?;
        // `valid` above guarantees the split; re-split to encode the part
        // segment for transport (PART_RE allows `#`, which a raw URL would
        // truncate as a fragment — the 2026-08-10 silent-collision bug).
        let (chat, part) = blob_ref.split_once('/').expect("validated above");
        let url = format!(
            "{}/blob/{}/{}",
            edge.url.trim_end_matches('/'),
            chat,
            encode_part_segment(part)
        );
        let res = self
            .inner
            .http
            .get(&url)
            .bearer_auth(&bearer)
            .send()
            .await
            .map_err(|e| {
                EngineError::Other(format!("sidecar fetch failed: {}", describe_http_error(e)))
            })?;
        if !res.status().is_success() {
            return Err(EngineError::Other(format!(
                "sidecar fetch: HTTP {}",
                res.status().as_u16()
            )));
        }
        res.text().await.map_err(|e| {
            EngineError::Other(format!(
                "sidecar body read failed: {}",
                describe_http_error(e)
            ))
        })
    }

    /// §2.2 writer discipline: we host a chat iff its workspace row's `deviceId` is
    /// ours; a chat with no row is claimable (claim-on-first-command). Without a
    /// wired workspace host (bare-DocHost tests) every open chat is ours — M2's
    /// behavior, now the degenerate case.
    fn is_host(&self, chat_id: &str) -> bool {
        self.workspace().is_none_or(|ws| ws.is_host(chat_id))
    }

    /// Chat-config harness when the workspace row carries one, else the default.
    pub(crate) fn harness_for(&self, chat_id: &str) -> HarnessId {
        self.workspace()
            .and_then(|ws| ws.chat_config(chat_id))
            .map(|config| config.harness)
            .unwrap_or(self.inner.config.default_harness)
    }

    /// The harness a request dispatches on: the request's own pick when it
    /// carries one (rides the command plane, immune to registry-row races),
    /// else [`Self::harness_for`].
    pub(crate) fn harness_for_request(
        &self,
        chat_id: &str,
        request: &luna_proto::RunRequest,
    ) -> HarnessId {
        request.harness.unwrap_or_else(|| self.harness_for(chat_id))
    }

    /// Drain pending commands (host-only): evaluate → mark processed BEFORE execute →
    /// execute → write the outcome as the sole outcome writer.
    pub async fn drain_commands(&self, handle: &Arc<ChatDocHandle>) {
        let Some(sessions) = self.sessions() else {
            return; // executor not wired yet (or retired); the set_sessions kick re-drains
        };
        if !self.is_host(&handle.chat_id) {
            return;
        }
        // Entries this pass decided to leave alone (processed dedupe hits).
        let mut skipped: HashSet<String> = HashSet::new();
        loop {
            let commands = match handle.doc.read_commands() {
                Ok(commands) => commands,
                Err(err) => {
                    tracing::warn!(chat = %handle.chat_id, error = %err, "command read failed");
                    return;
                }
            };
            // A chat2 row (including replay/checkpoint data) can set any
            // `issued_by` string. Only a locally persisted exact entry grants
            // dispatch. Reject an uncorrelated row before processed-ledger or
            // provider work; a collision with a local receipt stays pending
            // for manual reconciliation rather than rejecting that receipt.
            for entry in &commands {
                if entry.status != SessionCommandStatus::Pending || skipped.contains(&entry.id) {
                    continue;
                }
                let hash = match executable_row_hash(entry) {
                    Ok(hash) => hash,
                    Err(error) => {
                        tracing::warn!(chat = %handle.chat_id, command = %entry.id, %error,
                            "command authority hash unavailable; stopping drain");
                        return;
                    }
                };
                let authorized = match self.inner.store.local_executable_matches(
                    &handle.chat_id,
                    LocalExecutableKind::Command,
                    &entry.id,
                    &hash,
                ) {
                    Ok(authorized) => authorized,
                    Err(error) => {
                        tracing::warn!(chat = %handle.chat_id, command = %entry.id, %error,
                            "command authority unavailable; stopping drain");
                        return;
                    }
                };
                if authorized {
                    continue;
                }
                tracing::warn!(chat = %handle.chat_id, command = %entry.id,
                    "untrusted synced command quarantined before execution");
                let receipt = match self.task_command_receipt(&entry.id) {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        tracing::warn!(chat = %handle.chat_id, command = %entry.id, %error,
                            "command receipt unavailable; stopping drain");
                        return;
                    }
                };
                if receipt.is_none() {
                    if let Err(error) = handle.doc.set_command_status(
                        &entry.id,
                        SessionCommandStatus::Rejected,
                        Some("local command authority unavailable"),
                    ) {
                        tracing::warn!(chat = %handle.chat_id, command = %entry.id, %error,
                            "untrusted command rejection could not be published");
                    }
                }
                skipped.insert(entry.id.clone());
            }
            let is_processed = |id: &str| self.inner.store.is_processed(id).unwrap_or(false);
            // Dead-command sweep: Pending in the doc, consumed by the ledger,
            // and NOT mid-execution in this process — the crash window
            // between mark-processed and the outcome write. Left alone it is
            // a send no drain or retry can ever reach ("Sending…" forever,
            // 2026-08-19); terminalize it so the truth lands in the doc and
            // a user retry can mint a fresh attempt.
            for c in &commands {
                if c.status == SessionCommandStatus::Pending
                    && !skipped.contains(&c.id)
                    && is_processed(&c.id)
                    && !lock(&self.inner.executing).contains_key(&c.id)
                {
                    tracing::warn!(chat = %handle.chat_id, command = %c.id,
                        "command consumed but never resolved (crash mid-execute?); rejecting");
                    if let Err(error) = self.mark_task_command_unknown(
                        &c.id,
                        "the engine restarted after claiming this command; provider acceptance could not be proven",
                    ) {
                        tracing::warn!(chat = %handle.chat_id, command = %c.id, %error,
                            "failed to preserve ambiguous command receipt");
                    }
                    self.resolve_command(
                        handle,
                        &c.id,
                        SessionCommandStatus::Rejected,
                        Some("delivery outcome unknown — reconcile before retrying"),
                    );
                    skipped.insert(c.id.clone());
                }
            }
            let Some(entry) = commands
                .iter()
                .find(|c| {
                    c.status == SessionCommandStatus::Pending
                        && !skipped.contains(&c.id)
                        && !is_processed(&c.id)
                })
                .cloned()
            else {
                return;
            };
            let messages = handle.doc.read_entries().unwrap_or_default();
            let current_turn_id = messages.last().map(|m| m.id.clone());
            let turn_is_past = |turn_id: &str| messages.iter().any(|m| m.id == turn_id);
            let disposition = evaluate_command(
                &entry,
                &EvaluationContext {
                    is_processed: &is_processed,
                    now_ms: now_ms(),
                    entries: &commands,
                    current_turn_id: current_turn_id.as_deref(),
                    turn_is_past: &turn_is_past,
                },
            );
            // Queued-attachment gate (BEFORE the processed mark — a deferred
            // command must stay eligible): a Run/Steer naming `pending://`
            // refs whose bytes haven't landed on this device yet waits for
            // the transfer instead of running without its images. The wait is
            // bounded; past it the command fails loudly.
            if matches!(disposition, CommandDisposition::Execute) {
                let missing = self.missing_attachments(&entry);
                if !missing.is_empty() {
                    if now_ms().saturating_sub(entry.issued_at) < ATTACHMENT_WAIT_MAX_MS {
                        tracing::info!(chat = %handle.chat_id, command = %entry.id,
                            missing = missing.len(), "command deferred: attachment bytes in transit");
                        self.arm_attachment_wait(handle);
                        return; // preserve order; UploadCommit / the wait timer re-kick
                    }
                    if let Err(err) = self.inner.store.mark_processed(&entry.id) {
                        tracing::error!(chat = %handle.chat_id, error = %err,
                            "processed-ledger write failed; halting drain");
                        return;
                    }
                    tracing::warn!(chat = %handle.chat_id, command = %entry.id,
                        "command rejected: attachments never arrived");
                    self.resolve_command(
                        handle,
                        &entry.id,
                        SessionCommandStatus::Rejected,
                        Some("attachments never arrived"),
                    );
                    continue;
                }
            }
            // In-flight claim: guards the dead-command sweep (an id in
            // `executing` is alive, not crashed) and serializes racing
            // drains on the same entry.
            if !self.claim_executing(&handle.chat_id, &entry.id) {
                skipped.insert(entry.id.clone());
                continue;
            }
            // Mark BEFORE executing: a crash mid-execution must never double-run a
            // command whose side effect may already have happened.
            if let Err(err) = self.inner.store.mark_processed(&entry.id) {
                tracing::error!(chat = %handle.chat_id, error = %err, "processed-ledger write failed; halting drain");
                lock(&self.inner.executing).remove(&entry.id);
                return;
            }
            match disposition {
                CommandDisposition::Skip => {
                    skipped.insert(entry.id.clone());
                }
                CommandDisposition::Expired => {
                    self.resolve_command(handle, &entry.id, SessionCommandStatus::Expired, None);
                }
                CommandDisposition::Superseded => {
                    self.resolve_command(handle, &entry.id, SessionCommandStatus::Superseded, None);
                }
                CommandDisposition::Execute => {
                    let (status, resolution) = match self.execute(&sessions, handle, &entry).await {
                        Ok(outcome) => outcome,
                        Err(err) => (SessionCommandStatus::Rejected, Some(err.to_string())),
                    };
                    self.resolve_command(handle, &entry.id, status, resolution.as_deref());
                }
            }
            lock(&self.inner.executing).remove(&entry.id);
        }
    }

    /// The command's `pending://` attachment refs whose bytes are NOT on this
    /// device's disk yet. Empty when the command names none, when everything
    /// has landed, or when no uploads store is wired (tests) — absence of the
    /// subsystem must never wedge a queue.
    fn missing_attachments(&self, entry: &SessionCommandEntry) -> Vec<String> {
        let refs: Vec<String> = match &entry.payload {
            SessionCommandPayload::Run { request, .. } => request
                .attachments
                .iter()
                .filter(|p| crate::uploads::is_pending_ref(p))
                .cloned()
                .collect(),
            SessionCommandPayload::Steer {
                prompt,
                attachments,
                ..
            } => {
                let mut refs = crate::uploads::pending_refs_in(prompt);
                refs.extend(
                    attachments
                        .iter()
                        .filter(|p| crate::uploads::is_pending_ref(p))
                        .cloned(),
                );
                refs.sort();
                refs.dedup();
                refs
            }
            _ => Vec::new(),
        };
        if refs.is_empty() {
            return refs;
        }
        let Some(uploads) = self.inner.uploads.get() else {
            return Vec::new();
        };
        refs.into_iter()
            .filter(|r| uploads.resolve_pending(r).is_none())
            .collect()
    }

    /// Arm (once per chat) the deferred-command re-check loop: while a
    /// pending unprocessed command still waits on attachment bytes, re-drain
    /// on a cadence so the bounded wait actually expires even if every
    /// event-driven kick was missed.
    fn arm_attachment_wait(&self, handle: &Arc<ChatDocHandle>) {
        let chat = handle.chat_id.clone();
        if !lock(&self.inner.drain_waiting).insert(chat.clone()) {
            return;
        }
        let weak = Arc::downgrade(handle);
        let host = self.clone();
        self.spawn_worker(async move {
            loop {
                tokio::time::sleep(ATTACHMENT_WAIT_RECHECK).await;
                let Some(handle) = weak.upgrade() else { break };
                if !host.awaiting_attachments(&handle) {
                    break;
                }
                host.drain_commands(&handle).await;
                let Some(handle) = weak.upgrade() else { break };
                if !host.awaiting_attachments(&handle) {
                    break;
                }
            }
            lock(&host.inner.drain_waiting).remove(&chat);
        });
    }

    /// True while some pending, unprocessed command still waits on bytes.
    fn awaiting_attachments(&self, handle: &Arc<ChatDocHandle>) -> bool {
        let commands = handle.doc.read_commands().unwrap_or_default();
        commands.iter().any(|c| {
            c.status == SessionCommandStatus::Pending
                && !self.inner.store.is_processed(&c.id).unwrap_or(false)
                && !self.missing_attachments(c).is_empty()
        })
    }

    /// Rewrite a request's landed `pending://` refs to this device's absolute
    /// paths — in the attachments list AND the prompt text — so the harness
    /// (and the persisted user entry) see ordinary local files, exactly like
    /// the legacy pre-upload flow produced.
    fn resolve_request_attachments(&self, request: &mut luna_proto::RunRequest) {
        self.resolve_input_attachments(&mut request.prompt, &mut request.attachments);
    }

    fn resolve_input_attachments(&self, prompt: &mut String, attachments: &mut [String]) {
        let Some(uploads) = self.inner.uploads.get() else {
            return;
        };
        for path in attachments.iter_mut() {
            if let Some(abs) = uploads.resolve_pending(path) {
                *prompt = prompt.replace(path.as_str(), &abs);
                *path = abs;
            }
        }
    }

    /// [`Self::resolve_request_attachments`] for a bare prompt (Steer).
    fn resolve_prompt_attachments(&self, prompt: &str) -> String {
        let Some(uploads) = self.inner.uploads.get() else {
            return prompt.to_string();
        };
        let mut out = prompt.to_string();
        for r in crate::uploads::pending_refs_in(prompt) {
            if let Some(abs) = uploads.resolve_pending(&r) {
                out = out.replace(&r, &abs);
            }
        }
        out
    }

    /// Host-only outcome write (ledger rule 2).
    fn resolve_command(
        &self,
        handle: &ChatDocHandle,
        command_id: &str,
        status: SessionCommandStatus,
        resolution: Option<&str>,
    ) {
        if let Err(err) = handle
            .doc
            .set_command_status(command_id, status, resolution)
        {
            tracing::warn!(
                chat = %handle.chat_id,
                command = %command_id,
                error = %err,
                "command outcome write failed"
            );
        }
        let fence_conflict = resolution.and_then(|message| {
            message
                .strip_prefix("staleTurn: ")
                .map(|message| (CommandReasonCode::StaleTurn, message))
                .or_else(|| {
                    message
                        .strip_prefix("staleBindingGeneration: ")
                        .map(|message| (CommandReasonCode::StaleBindingGeneration, message))
                })
        });
        let receipt_status = if fence_conflict.is_some() {
            CommandReceiptStatus::Conflict
        } else {
            match status {
                SessionCommandStatus::Applied => CommandReceiptStatus::Applied,
                SessionCommandStatus::Rejected | SessionCommandStatus::Cancelled => {
                    CommandReceiptStatus::Rejected
                }
                SessionCommandStatus::Superseded => CommandReceiptStatus::Superseded,
                SessionCommandStatus::Expired => CommandReceiptStatus::Failed,
                SessionCommandStatus::Pending => return,
            }
        };
        let Ok(Some(prior)) = self.task_command_receipt(command_id) else {
            return;
        };
        // Run/steer receipts carry a correlation before dispatch. Their doc
        // command is locally applied once it reaches the execution lane, but
        // the durable receipt remains pending until SessionStarted/Steered
        // proves provider acceptance for that exact correlation.
        if receipt_status == CommandReceiptStatus::Applied && prior.correlation_id.is_some() {
            return;
        }
        // A dead-command sweep already marked the durable outcome unknown. Its
        // doc row is terminalized only to remove a stuck spinner; never turn
        // that ambiguity into a replayable rejection receipt.
        if prior.status == CommandReceiptStatus::Pending
            && prior.committed_at_ms.is_some()
            && prior
                .reason
                .as_ref()
                .is_some_and(|reason| reason.code == CommandReasonCode::ProviderAcceptanceUnknown)
        {
            return;
        }
        let active_turn = self
            .sessions()
            .and_then(|sessions| sessions.active_turn_id(&handle.chat_id));
        let reason = if let Some((code, message)) = fence_conflict {
            Some(CommandReceiptReason {
                code,
                message: message.into(),
            })
        } else {
            resolution.map(|message| CommandReceiptReason {
                code: if receipt_status == CommandReceiptStatus::Rejected {
                    CommandReasonCode::OperationRejected
                } else {
                    CommandReasonCode::OperationFailed
                },
                message: message.into(),
            })
        };
        let recovery = fence_conflict
            .is_some()
            .then_some("Reload task state; never replay this control against a replacement binding or turn.")
            .or(resolution);
        let stop_command = if receipt_status == CommandReceiptStatus::Applied {
            match handle.doc.read_commands() {
                Ok(commands) => commands.into_iter().any(|command| {
                    command.id == command_id
                        && matches!(command.payload, SessionCommandPayload::Interrupt {})
                }),
                Err(error) => {
                    tracing::error!(chat = %handle.chat_id, command = %command_id, %error,
                        "Stop receipt finalization could not read its durable command");
                    return;
                }
            }
        } else {
            false
        };
        let stop_recovery = if stop_command {
            let Some(sessions) = self.sessions() else {
                tracing::error!(chat = %handle.chat_id, command = %command_id,
                    "Stop receipt finalization lost the sessions recovery authority");
                return;
            };
            match sessions.prepare_hold_recovery(
                &handle.chat_id,
                "Recovery held by Stop. Queued work and partial output were preserved.",
            ) {
                Ok(change) => change,
                Err(error) => {
                    tracing::error!(chat = %handle.chat_id, command = %command_id, %error,
                        "Stop receipt finalization could not prepare its durable recovery hold");
                    return;
                }
            }
        } else {
            None
        };
        let result = if let Some(change) = stop_recovery.as_ref() {
            match change.storage_transition() {
                Ok(transition) => self.complete_task_command_with_recovery(
                    command_id,
                    receipt_status,
                    true,
                    prior.snapshot.binding_id.as_deref(),
                    prior.snapshot.binding_generation,
                    active_turn.as_deref(),
                    reason,
                    recovery,
                    &transition,
                ),
                Err(error) => Err(error),
            }
        } else {
            self.complete_task_command(
                command_id,
                receipt_status,
                receipt_status == CommandReceiptStatus::Applied,
                prior.snapshot.binding_id.as_deref(),
                prior.snapshot.binding_generation,
                active_turn.as_deref(),
                reason,
                recovery,
            )
        };
        if let Err(error) = result {
            tracing::error!(chat = %handle.chat_id, command = %command_id, %error,
                "task command receipt finalization failed");
        } else if let Some(change) = stop_recovery
            && let Some(sessions) = self.sessions()
        {
            sessions.publish_committed_recovery(&change.next);
        }
    }

    async fn execute(
        &self,
        sessions: &SessionsEngine,
        handle: &Arc<ChatDocHandle>,
        entry: &SessionCommandEntry,
    ) -> Result<(SessionCommandStatus, Option<String>), EngineError> {
        let chat_id = &handle.chat_id;
        let original_w = self
            .inner
            .store
            .command_workspace_repair_revision(&entry.id)?
            .or(self
                .inner
                .store
                .local_executable_authority(chat_id, LocalExecutableKind::Command, &entry.id)?
                .map(|grant| grant.workspace_repair_revision))
            .unwrap_or(0);
        if original_w != self.inner.store.workspace_repair_revision(chat_id)? {
            return Ok((SessionCommandStatus::Rejected,
                Some("workspaceRepairChanged: this command belongs to an earlier workspace association".into())));
        }
        if let Some(receipt) = self.task_command_receipt(&entry.id)? {
            if let Some(expected) = receipt.snapshot.binding_generation {
                let current = sessions.task_binding_generation(chat_id);
                if current != Some(expected) {
                    return Ok((
                        SessionCommandStatus::Rejected,
                        Some(format!(
                            "staleBindingGeneration: expected {expected}, current {current:?}"
                        )),
                    ));
                }
            }
            if matches!(
                &entry.payload,
                SessionCommandPayload::Steer { .. } | SessionCommandPayload::Interrupt {}
            ) && let Some(expected) = receipt.snapshot.active_turn_id.as_deref()
            {
                let current = sessions.active_turn_id(chat_id);
                if current.as_deref() != Some(expected) {
                    return Ok((
                        SessionCommandStatus::Rejected,
                        Some(format!(
                            "staleTurn: expected {expected}, current {current:?}"
                        )),
                    ));
                }
            }
        }
        match &entry.payload {
            SessionCommandPayload::Run {
                request,
                message_id,
            } => {
                let mut request = request.clone();
                // Queued-attachment refs (`pending://`) resolve to this
                // host's absolute paths before anything persists or
                // dispatches — the drain already gated on the bytes being
                // present, so every ref resolves here.
                self.resolve_request_attachments(&mut request);
                // Worktree directive (WorktreeSpec): materialize on THIS host at
                // drain time — the durable command plane replaces the sender's
                // old blocking CreateWorktree relay RPC, whose lost reply wedged
                // the composer on "Sending…" while the run proceeded anyway.
                // `take()` resolves the request before dispatch, so the journal
                // and steer→new-turn fallbacks reuse the created path instead of
                // minting another checkout.
                let mut managed_materialization = false;
                let fresh_worktree = match request.worktree.take() {
                    Some(spec) => {
                        let context = sessions
                            .prepare_workspace_materialization(chat_id, &spec)
                            .await?;
                        let (cwd, fresh) = self.materialize_worktree(chat_id, &spec).await?;
                        if let Some(context) = context {
                            let fence = luna_sync::WorkspaceMaterializationFence {
                                expected_workspace_repair_revision: original_w,
                                original_command_hash: executable_row_hash(entry)?,
                            };
                            sessions
                                .commit_workspace_materialization(
                                    context,
                                    &entry.id,
                                    &fence,
                                    &cwd,
                                    fresh.as_ref().map(|worktree| worktree.branch.as_str()),
                                )
                                .await
                                .map_err(|error| EngineError::Other(error.message().to_string()))?;
                            managed_materialization = true;
                        }
                        request.cwd = cwd;
                        fresh
                    }
                    None => None,
                };
                // Claim-on-first-command: a run for a chat with no workspace row
                // creates the row under our device id (we are about to host it).
                if let Some(ws) = self.workspace() {
                    ws.claim_chat(chat_id, Some(&request.cwd))?;
                    // A pre-existing row (the client's createChat raced ahead)
                    // still carries the repo folder — repoint it at the fresh
                    // worktree, and stamp the actual `luna/<name>` branch so
                    // the footer and the title-rename flow see it.
                    if !managed_materialization && let Some(wt) = &fresh_worktree {
                        ws.set_chat_cwd(chat_id, &wt.path)?;
                        if let Err(err) = ws.set_chat_branch(chat_id, &wt.branch) {
                            tracing::warn!(chat = %chat_id, error = %err, "worktree branch stamp failed");
                        }
                        // Publish the existing checkout receipt durably before
                        // any captured setup can receive its launch handshake.
                        ws.flush_checked()?;
                    }
                }
                if let Some(commands) = self.inner.project_commands.get() {
                    if fresh_worktree.is_some() {
                        commands.setup_new_worktree(chat_id, &request.cwd).await?;
                    }
                    commands.wait_for_setup(chat_id, &request.cwd).await?;
                }
                let harness = self.harness_for_request(chat_id, &request);
                // A row with no config renders no harness glyph (and every
                // later dispatch falls back to the engine default), so stamp
                // what this run actually executes with. Claimed rows and
                // catalog-not-loaded createChats both land here; the racing
                // real createChat carries the same picked values.
                if let Some(ws) = self.workspace()
                    && ws.chat_config(chat_id).is_none()
                {
                    let config = luna_proto::ChatConfig {
                        harness,
                        provider_account_id: request.provider_account_id.clone(),
                        model: request.model.clone(),
                        reasoning: request.reasoning,
                        model_options: request.model_options.clone(),
                        sandbox: request.sandbox,
                        permission_mode: request.permission_mode,
                        effective_permission_mode: request
                            .permission_policy
                            .as_ref()
                            .map(|policy| policy.effective_mode)
                            .unwrap_or(luna_proto::PermissionMode::Ask),
                        permission_ceiling_source: request
                            .permission_policy
                            .as_ref()
                            .and_then(|policy| policy.ceiling_source),
                        permission_degradation_reason: request
                            .permission_policy
                            .as_ref()
                            .and_then(|policy| policy.degradation_reason),
                        permission_audit: Vec::new(),
                    };
                    if let Err(err) = ws.set_chat_config(chat_id, &config) {
                        tracing::warn!(chat = %chat_id, error = %err, "run-config backfill failed");
                    }
                }
                // Timestamp canonicalization: the user message lands in
                // history at the moment the user SENT it (the entry's
                // issued_at, clamped against clock skew) — not whenever this
                // host got around to draining a queued command. Idempotent by
                // id, so the dispatch path's own execution-time write dedupes
                // to a no-op.
                if let Err(err) = handle.write_user_message(
                    message_id,
                    &request.prompt,
                    entry.issued_at.min(now_ms()),
                ) {
                    tracing::warn!(chat = %chat_id, error = %err, "canonical user-message write failed");
                }
                self.dispatch_with_source_context(
                    sessions,
                    chat_id,
                    harness,
                    request,
                    Some(message_id.clone()),
                    original_w,
                )
                .await?;
                // A fresh user-authored turn is the deliberate action that
                // thaws a queue frozen by Cancel. Clear only after dispatch
                // succeeds so a failed send cannot silently unfreeze it.
                handle.queue_paused.store(false, Ordering::Release);
                Ok((SessionCommandStatus::Applied, None))
            }
            SessionCommandPayload::Steer {
                prompt,
                message_id,
                attachments,
                input_items,
            } => {
                self.deliver_prompt_with_inputs(
                    sessions,
                    handle,
                    prompt,
                    attachments,
                    input_items,
                    message_id.clone(),
                    entry.issued_at,
                    original_w,
                )
                .await
            }
            SessionCommandPayload::Interrupt {} => {
                self.interrupt_and_pause_queue(sessions, handle).await?;
                Ok((SessionCommandStatus::Applied, None))
            }
            SessionCommandPayload::RespondApproval {
                request_id,
                decision,
            } => {
                if sessions.is_luna_pending_approval(chat_id, request_id) {
                    return Ok((
                        SessionCommandStatus::Rejected,
                        Some("LUNA approval requires a trusted local UI response".into()),
                    ));
                }
                if sessions.respond_approval(chat_id, request_id, *decision)? {
                    return Ok((SessionCommandStatus::Applied, None));
                }
                // Provider disconnect/restart drops the only response channel.
                // A recovered approval remains visible in the transcript, but
                // answering it can only close it as denied; never replay an
                // approval as a new prompt or infer that an action succeeded.
                let recoverable = recoverable_provider_approval(&handle.doc, request_id);
                if !recoverable {
                    return Ok((
                        SessionCommandStatus::Rejected,
                        Some("no pending approval request".into()),
                    ));
                }
                handle.doc.resolve_input(request_id)?;
                Ok((
                    SessionCommandStatus::Applied,
                    Some("provider disconnected; approval denied safely".into()),
                ))
            }
            SessionCommandPayload::RespondInput {
                request_id,
                answers,
            } => {
                if sessions.is_luna_pending_approval(chat_id, request_id) {
                    return Ok((
                        SessionCommandStatus::Rejected,
                        Some("LUNA approval requires a trusted local UI response".into()),
                    ));
                }
                if sessions.respond_input(chat_id, request_id, answers.clone())? {
                    return Ok((SessionCommandStatus::Applied, None));
                }
                // No live resolver. Only a request id the doc shows as an
                // OPEN question on a SETTLED entry gets the orphan fallback:
                // a mismatched or already-resolved id is a stale/buggy answer
                // and must still reject, and a still-streaming entry's
                // question belongs to the live run (a just-consumed resolver
                // racing a second answer must not spawn a duplicate turn).
                let questions = handle.doc.read_entries().ok().and_then(|entries| {
                    entries
                        .iter()
                        .rev()
                        .filter(|e| e.status != Some(MessageStatus::Streaming))
                        .find_map(|e| {
                            e.parts.iter().find_map(|p| match p {
                                MessagePart::Input {
                                    request_id: rid,
                                    questions,
                                    resolved: false,
                                    ..
                                } if rid == request_id => Some(questions.clone()),
                                _ => None,
                            })
                        })
                });
                let Some(questions) = questions else {
                    return Ok((
                        SessionCommandStatus::Rejected,
                        Some("no pending input request".into()),
                    ));
                };
                if questions.iter().any(|question| {
                    question
                        .approval
                        .as_ref()
                        .is_some_and(luna_proto::ApprovalRequest::is_luna_cross_task)
                }) {
                    return Ok((
                        SessionCommandStatus::Rejected,
                        Some("LUNA approval requires a trusted local UI response".into()),
                    ));
                }
                if questions.iter().any(|question| question.mcp.is_some()) {
                    // A restart cannot recreate the server's exact resolver or
                    // authorization URL. Close the stale UI only on an explicit
                    // decline/cancel; never resume the model with form values.
                    let action = luna_proto::mcp_response(&questions, answers)
                        .map_err(|message| EngineError::Other(message.into()))?;
                    if action.get("action").and_then(serde_json::Value::as_str) == Some("accept") {
                        return Ok((
                            SessionCommandStatus::Rejected,
                            Some("MCP request expired; run the tool again".into()),
                        ));
                    }
                    handle.doc.resolve_input(request_id)?;
                    return Ok((
                        SessionCommandStatus::Applied,
                        Some("provider disconnected; MCP request closed locally".into()),
                    ));
                }
                if questions.iter().any(|question| question.approval.is_some()) {
                    // A recovered approval has no provider response channel.
                    // Only RespondApproval may close it (as a safe denial);
                    // never turn arbitrary input labels into a resumed prompt.
                    return Ok((
                        SessionCommandStatus::Rejected,
                        Some("typed approval requires respondApproval".into()),
                    ));
                }
                // The run died under the question (engine restart, crash).
                // The question is still open in the doc and the command is
                // durable, so honor it anyway — stamp the part resolved and
                // deliver the answers as the next (resumed) turn, the same
                // fallback a dead-run steer takes. The question UI stays up
                // until the user answers (user requirement); this is what
                // makes that answer still WORK.
                let request = sessions
                    .last_request(chat_id)
                    .or_else(|| self.request_from_chat_row(chat_id, ""));
                let Some(mut request) = request else {
                    return Ok((
                        SessionCommandStatus::Rejected,
                        Some("no pending input request and no prior run config".into()),
                    ));
                };
                request.review = None;
                request.prompt = respond_input_prompt(&questions, answers);
                request.resume = None; // dispatch re-derives the harness session
                request.attachments = Vec::new();
                request.input_items.clear();
                if let Err(err) = handle.doc.resolve_input(request_id) {
                    tracing::warn!(chat = %chat_id, request = %request_id, error = %err,
                        "orphaned input resolve failed");
                }
                let harness = self.harness_for_request(chat_id, &request);
                self.dispatch_with_source_context(
                    sessions, chat_id, harness, request, None, original_w,
                )
                .await?;
                Ok((
                    SessionCommandStatus::Applied,
                    Some("answered as new turn".into()),
                ))
            }
            SessionCommandPayload::RespondAsyncQuestion {
                message_id,
                answers,
            } => {
                // Unlike RespondInput, this provider tool has already
                // returned. The card is transcript metadata; its response is
                // either a durable dismissal or an ordinary user steer.
                let question_set = handle.doc.read_entries()?.into_iter().rev().find_map(|e| {
                    e.parts.into_iter().find_map(|p| match p {
                        MessagePart::AsyncQuestion {
                            id,
                            questions,
                            resolved: false,
                        } if id == *message_id => Some(questions),
                        _ => None,
                    })
                });
                let Some(questions) = question_set else {
                    return Ok((
                        SessionCommandStatus::Rejected,
                        Some("no open async question".into()),
                    ));
                };
                let prompt = match async_answer_prompt(&questions, answers) {
                    Ok(prompt) => prompt,
                    Err(reason) => {
                        return Ok((SessionCommandStatus::Rejected, Some(reason.into())));
                    }
                };
                let Some(prompt) = prompt else {
                    handle.doc.resolve_async_question(message_id)?;
                    return Ok((
                        SessionCommandStatus::Applied,
                        Some("question skipped".into()),
                    ));
                };
                let (status, detail) = self
                    .deliver_prompt(
                        sessions,
                        handle,
                        &prompt,
                        Some(entry.id.clone()),
                        entry.issued_at,
                        original_w,
                    )
                    .await?;
                if status == SessionCommandStatus::Applied {
                    handle.doc.resolve_async_question(message_id)?;
                }
                Ok((status, detail))
            }
        }
    }

    /// Put a typed prompt in front of a live agent: steer it in, or — with no
    /// live steerable run — deliver the durable command as the next turn.
    /// After an engine restart `last_request` is empty too, so rebuild the run
    /// config from the chat's workspace row (luna derived dispatch config from
    /// the chat row the same way — sessions.ts:601-620); dispatch's engine-owned
    /// resume then reattaches the prior harness conversation.
    ///
    /// A working agent that only takes prompts at a turn boundary is the
    /// exception: see [`Self::held_until_turn_end`].
    async fn deliver_prompt(
        &self,
        sessions: &SessionsEngine,
        handle: &Arc<ChatDocHandle>,
        prompt: &str,
        message_id: Option<String>,
        issued_at: i64,
        original_w: u64,
    ) -> Result<(SessionCommandStatus, Option<String>), EngineError> {
        self.deliver_prompt_with_inputs(
            sessions,
            handle,
            prompt,
            &[],
            &[],
            message_id,
            issued_at,
            original_w,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn deliver_prompt_with_inputs(
        &self,
        sessions: &SessionsEngine,
        handle: &Arc<ChatDocHandle>,
        prompt: &str,
        attachments: &[String],
        input_items: &[luna_proto::ComposerInput],
        message_id: Option<String>,
        issued_at: i64,
        original_w: u64,
    ) -> Result<(SessionCommandStatus, Option<String>), EngineError> {
        let chat_id = &handle.chat_id;
        // Keep upstream's queued-attachment rewrite and send-time canonical
        // history while preserving the personal cut's turn-boundary queue.
        let mut prompt = self.resolve_prompt_attachments(prompt);
        let mut attachments = attachments.to_vec();
        self.resolve_input_attachments(&mut prompt, &mut attachments);
        if self.harness_for(chat_id) == luna_proto::HarnessId::Codex {
            luna_harness::validate_composer_inputs(&attachments, input_items)?;
        } else if !input_items.is_empty() {
            return Err(EngineError::Other("Native plugin, app and skill selections require Codex; keep the draft and choose Codex or remove them.".into()));
        }
        if self.held_until_turn_end(
            sessions,
            chat_id,
            &prompt,
            &attachments,
            input_items,
            message_id.as_deref(),
        )? {
            return Ok((
                SessionCommandStatus::Applied,
                Some("held until the turn ends".into()),
            ));
        }
        if let Some(message_id) = message_id.as_deref()
            && let Err(err) =
                handle.write_user_message(message_id, &prompt, issued_at.min(now_ms()))
        {
            tracing::warn!(chat = %chat_id, error = %err, "canonical user-message write failed");
        }
        match sessions
            .steer_with_inputs_at_workspace_revision(
                chat_id,
                &prompt,
                &attachments,
                input_items,
                message_id.clone(),
                original_w,
            )
            .await?
        {
            SteerOutcome::Accepted => {
                handle.queue_paused.store(false, Ordering::Release);
                Ok((SessionCommandStatus::Applied, None))
            }
            SteerOutcome::NotSteerable => {
                let request = sessions
                    .last_request(chat_id)
                    .or_else(|| self.request_from_chat_row(chat_id, &prompt));
                let Some(mut request) = request else {
                    return Ok((
                        SessionCommandStatus::Rejected,
                        Some("no live run and no prior run config".into()),
                    ));
                };
                request.review = None;
                request.prompt = prompt;
                request.resume = None; // dispatch re-derives the harness session
                // Replace the previous turn's envelope with this input's
                // staged images and exact native selections.
                request.attachments = attachments;
                request.input_items = input_items.to_vec();
                let harness = self.harness_for_request(chat_id, &request);
                self.dispatch_with_source_context(
                    sessions, chat_id, harness, request, message_id, original_w,
                )
                .await?;
                handle.queue_paused.store(false, Ordering::Release);
                Ok((
                    SessionCommandStatus::Applied,
                    Some("queued as new turn".into()),
                ))
            }
            SteerOutcome::Rejected => {
                if let Some(message_id) = message_id.as_deref() {
                    handle
                        .doc
                        .remove_rejected_user_message(message_id, &prompt)?;
                }
                Ok((
                    SessionCommandStatus::Rejected,
                    Some("the provider rejected native in-turn steering".into()),
                ))
            }
            SteerOutcome::Busy => {
                if let Some(message_id) = message_id.as_deref() {
                    handle
                        .doc
                        .remove_rejected_user_message(message_id, &prompt)?;
                }
                Ok((
                    SessionCommandStatus::Rejected,
                    Some("a previous native steer is still settling".into()),
                ))
            }
            SteerOutcome::Unknown => Ok((
                SessionCommandStatus::Pending,
                Some("native steer acceptance is unknown; it was not replayed".into()),
            )),
        }
    }

    /// Put `prompt` in the pending-message queue instead of a run mailbox when
    /// the agent is working and takes prompts only between turns. `true` when it
    /// was queued.
    ///
    /// A turn-boundary agent has no mid-turn mailbox: what it has is a next
    /// prompt. Posting into the run's mailbox anyway made delivery depend on how
    /// the turn ended — a turn that was interrupted or errored discarded the
    /// mailbox, and the message sat in the transcript looking sent, unread. The
    /// queue is the honest home: shown as pending rather than sent, editable,
    /// and flushed by the turn-end watcher, which is where [`Self::drain_queue`]
    /// already sends a typed message for exactly this agent. `steers_mid_turn`
    /// is a catalog lookup — no spawn, no probe.
    fn held_until_turn_end(
        &self,
        sessions: &SessionsEngine,
        chat_id: &str,
        prompt: &str,
        attachments: &[String],
        input_items: &[luna_proto::ComposerInput],
        message_id: Option<&str>,
    ) -> Result<bool, EngineError> {
        // A persistent session parked BETWEEN turns DOES take the next prompt
        // through its mailbox (the warm-child fast path), so the question is
        // whether a turn is in flight — including one parked on a question,
        // which is the state a question's own follow-up arrives in. An empty
        // prompt is no message to queue.
        if !sessions.turn_in_flight(chat_id)
            || (prompt.trim().is_empty() && attachments.is_empty() && input_items.is_empty())
            || sessions.steers_mid_turn(self.harness_for(chat_id))
        {
            return Ok(false);
        }
        let handle = self.open(chat_id)?;
        let message_id = message_id.map(str::to_string).unwrap_or_else(new_id);
        let execution = self
            .workspace()
            .and_then(|workspace| workspace.chat_config(chat_id));
        self.persist_queue_projection(&handle, &format!("turn-end:{message_id}"), |doc| {
            doc.push_queued(&QueuedMessage {
                input_items: input_items.to_vec(),
                id: message_id,
                text: prompt.to_string(),
                attachments: attachments.to_vec(),
                hold_for_turn_end: false,
                follow_up_behavior: luna_proto::FollowUpBehavior::Queue,
                hold_reason: None,
                issued_by: self.inner.config.device_id.clone(),
                issued_at: now_ms(),
                edited_at: None,
                revision: 1,
                execution,
                status: QueueMessageStatus::Held,
                delivery_gate: None,
            })?;
            Ok(())
        })?;
        Ok(true)
    }

    async fn capture_source_context(&self, cwd: &str) -> Option<ConversationSourceContext> {
        let repos = self.inner.repos.get()?;
        let path = Path::new(cwd);
        let identity = repos.checkout_identity(path).await.ok()?;
        let branch = repos.current_branch(path).await.ok()?;
        let head_sha = repos.head_sha(path).await.ok().flatten();
        Some(ConversationSourceContext {
            checkout_id: identity.id,
            repo_root: identity.root.to_string_lossy().into_owned(),
            cwd: cwd.to_string(),
            branch,
            head_sha,
            observed_at: chrono::Utc::now(),
        })
    }

    /// Every fresh harness dispatch crosses this boundary, including
    /// steer/input fallbacks and crash recovery. Capture immediately before
    /// dispatch so the conversation records the checkout the harness will
    /// actually observe, rather than whichever checkout state was current on
    /// an earlier turn.
    pub(crate) async fn dispatch_with_source_context(
        &self,
        sessions: &SessionsEngine,
        chat_id: &str,
        harness: HarnessId,
        request: luna_proto::RunRequest,
        message_id: Option<String>,
        original_w: u64,
    ) -> Result<String, EngineError> {
        if let Some(workspace) = self.workspace()
            && let Some(context) = self.capture_source_context(&request.cwd).await
            && let Err(err) = workspace.set_chat_source_context(chat_id, &context)
        {
            tracing::warn!(chat = %chat_id, error = %err, "conversation source stamp failed");
        }
        sessions
            .dispatch_at_workspace_revision(chat_id, harness, request, message_id, original_w)
            .await
    }

    /// Create (or reuse) the isolated worktree a Run's [`luna_proto::WorktreeSpec`]
    /// asks for, returning the resolved cwd plus the fresh worktree when one was
    /// actually created. Reuse guard: a chat whose row already points inside a
    /// linked worktree of the same repo keeps it — a duplicate Run (client retry
    /// after a lost ack, ledger reset) must not mint a second checkout.
    async fn materialize_worktree(
        &self,
        chat_id: &str,
        spec: &luna_proto::WorktreeSpec,
    ) -> Result<(String, Option<luna_proto::Worktree>), EngineError> {
        if let Some(ws) = self.workspace()
            && let Ok(Some(chat)) = ws.chat(chat_id)
            && let Some(cwd) = chat.cwd
            && cwd != spec.repo_path
            && crate::workspace_host::linked_worktree_root(std::path::Path::new(&cwd)).as_deref()
                == Some(spec.repo_path.as_str())
        {
            tracing::info!(chat = %chat_id, cwd = %cwd, "worktree spec: reusing the chat's existing worktree");
            return Ok((cwd, None));
        }
        let repos = self
            .inner
            .repos
            .get()
            .ok_or_else(|| EngineError::Other("repos engine not wired".into()))?;
        let worktree = repos
            .create_worktree(std::path::Path::new(&spec.repo_path), &spec.base)
            .await?;
        if let Some(commands) = self.inner.project_commands.get() {
            commands
                .prepare_new_worktree(chat_id, &worktree.path)
                .await?;
        }
        tracing::info!(
            chat = %chat_id,
            path = %worktree.path,
            branch = %worktree.branch,
            "worktree materialized for run"
        );
        Ok((worktree.path.clone(), Some(worktree)))
    }

    /// A steer-turned-run with no in-process `last_request` (engine restarted
    /// since the last turn): rebuild the run config from the chat's workspace
    /// row — cwd from the row, model/reasoning/options/sandbox from its config
    /// (composer defaults otherwise). `None` without a workspace host or row.
    // (Also the RespondInput dead-run fallback's config source.)
    pub(crate) fn request_from_chat_row(
        &self,
        chat_id: &str,
        prompt: &str,
    ) -> Option<luna_proto::RunRequest> {
        let workspace = self.workspace()?;
        let chat = match workspace.chat(chat_id) {
            Ok(chat) => chat?,
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "workspace chat read failed");
                return None;
            }
        };
        let config = chat.config;
        Some(luna_proto::RunRequest {
            review: None,
            input_items: Vec::new(),
            prompt: prompt.to_string(),
            harness: config.as_ref().map(|c| c.harness),
            provider_account_id: config.as_ref().and_then(|c| c.provider_account_id.clone()),
            model: config.as_ref().and_then(|c| c.model.clone()),
            reasoning: config.as_ref().and_then(|c| c.reasoning),
            model_options: config
                .as_ref()
                .map(|c| c.model_options.clone())
                .unwrap_or_default(),
            cwd: chat.cwd.unwrap_or_default(),
            sandbox: config
                .as_ref()
                .map(|c| c.sandbox)
                .unwrap_or(luna_proto::SandboxLevel::WorkspaceWrite),
            auto_approve: false,
            permission_mode: config
                .as_ref()
                .map(|config| config.permission_mode)
                .unwrap_or_default(),
            parent_permission_ceiling: None,
            environment_permission_ceiling: None,
            permission_policy: None,
            attachments: Vec::new(),
            resume: None,
            worktree: None,
        })
    }

    fn save_snapshot(&self, handle: &ChatDocHandle) {
        if let Err(error) = self.save_snapshot_checked(handle) {
            tracing::warn!(chat = %handle.chat_id, %error, "snapshot save failed");
        }
    }

    fn save_snapshot_checked(&self, handle: &ChatDocHandle) -> Result<(), EngineError> {
        let _projection = lock(&handle.queue_projection_lock);
        if handle
            .durable_projection_uninstalled
            .load(Ordering::Acquire)
        {
            return Err(EngineError::Other(
                "snapshot save skipped after an uninstalled durable projection".into(),
            ));
        }
        if handle.retired.load(Ordering::Relaxed) {
            // A chat2 seed replaced this lineage on disk; persisting this
            // handle's fat doc would clobber the thin one. But retired with
            // NO thin lineage on disk (a stuck handle from the born-gen2
            // race) means this doc is its transcript's only copy — skipping
            // the save turned an app quit into total loss (2026-08-11);
            // persist it, and let the adopt path convert it on reopen.
            let thin_on_disk = matches!(
                self.inner.store.load_snapshot_with_cursor(&handle.chat_id)?,
                Some((_, _, epoch)) if epoch >= crate::chat2_host::CHAT2_DOC_EPOCH
            );
            if thin_on_disk {
                return Ok(());
            }
        }
        let bytes = handle.doc.export_snapshot()?;
        self.inner.store.save_snapshot(&handle.chat_id, &bytes)?;
        handle.snapshot_bytes.store(bytes.len(), Ordering::Relaxed);
        Ok(())
    }

    /// Persist every open doc now (shutdown path; bypasses the debounce).
    pub fn flush_all(&self) {
        let handles: Vec<_> = lock(&self.inner.handles).values().cloned().collect();
        for handle in handles {
            self.save_snapshot(&handle);
        }
    }

    pub(crate) fn flush_all_checked(&self) -> Result<(), EngineError> {
        let handles: Vec<_> = lock(&self.inner.handles).values().cloned().collect();
        let mut first_error = None;
        for handle in handles {
            if let Err(error) = self.save_snapshot_checked(&handle) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Close all account-scoped room memberships before graceful engine
    /// draining. Auth-aware join supervisors will not install a late client.
    pub fn disconnect_edge(&self) {
        self.inner.edge_disconnected.store(true, Ordering::Release);
        let handles: Vec<_> = lock(&self.inner.handles).values().cloned().collect();
        for handle in handles {
            lock(&handle.chat2).take();
            // Retain the durable subscription through agent shutdown cleanup.
        }
    }
}

/// Format a late answer as ordinary user input. Empty answers mean Skip;
/// malformed question ids must not silently turn an intended answer into Skip.
fn async_answer_prompt(
    questions: &[luna_proto::AsyncUserInputQuestion],
    answers: &[UserInputAnswer],
) -> Result<Option<String>, &'static str> {
    let mut lines = Vec::new();
    for answer in answers {
        let index = answer
            .question_id
            .parse::<usize>()
            .map_err(|_| "invalid async question id")?;
        let title = questions
            .get(index)
            .ok_or("invalid async question id")?
            .title
            .trim();
        let labels: Vec<_> = answer
            .labels
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .collect();
        if !labels.is_empty() {
            lines.push(format!("{title}: {}", labels.join(", ")));
        }
    }
    Ok((!lines.is_empty())
        .then(|| format!("Answering your earlier question:\n{}", lines.join("\n"))))
}

#[cfg(test)]
mod async_question_tests {
    use super::async_answer_prompt;
    use luna_proto::{AsyncUserInputQuestion, UserInputAnswer};

    #[test]
    fn answer_becomes_contextual_steering_and_skip_has_no_prompt() {
        let questions = vec![AsyncUserInputQuestion {
            title: "Which sample should I use?".into(),
            options: Some(vec!["A".into(), "B".into()]),
        }];
        assert_eq!(async_answer_prompt(&questions, &[]).unwrap(), None);
        let answer = UserInputAnswer {
            question_id: "0".into(),
            labels: vec!["B".into()],
        };
        assert_eq!(
            async_answer_prompt(&questions, &[answer]).unwrap(),
            Some("Answering your earlier question:\nWhich sample should I use?: B".into())
        );
        assert!(
            async_answer_prompt(
                &questions,
                &[UserInputAnswer {
                    question_id: "9".into(),
                    labels: vec!["A".into()],
                }]
            )
            .is_err()
        );
    }
}

/// An orphaned provider approval can only be closed as denied. A LUNA marker
/// is never recoverable through the generic command path, even after restart.
fn recoverable_provider_approval(doc: &SessionDoc, request_id: &str) -> bool {
    let Ok(entries) = doc.read_entries() else {
        return false;
    };
    for entry in entries.iter().rev() {
        for part in entry.parts.iter().rev() {
            if let MessagePart::Input {
                request_id: pending_id,
                questions,
                resolved,
                ..
            } = part
                && pending_id == request_id
            {
                return !resolved
                    && questions.iter().any(|question| question.approval.is_some())
                    && !questions.iter().any(|question| {
                        question
                            .approval
                            .as_ref()
                            .is_some_and(luna_proto::ApprovalRequest::is_luna_cross_task)
                    });
            }
        }
    }
    false
}

#[cfg(test)]
mod luna_approval_recovery_tests {
    use super::*;
    use crate::registry::HarnessRegistry;
    use crate::run_journal::RunJournal;

    fn approval_question(kind: luna_proto::ApprovalActionKind) -> UserInputQuestion {
        UserInputQuestion {
            id: "question".into(),
            header: "Approval".into(),
            question: "Allow?".into(),
            options: vec!["Deny".into(), "Allow once".into()],
            multi_select: false,
            mcp: None,
            approval: Some(luna_proto::ApprovalRequest {
                provider_request_id: "claimed-local-ui".into(),
                provider: HarnessId::Codex,
                turn_id: Some("turn-a".into()),
                machine_name: "synthetic".into(),
                action_kind: kind,
                summary: "synthetic".into(),
                requested_scope: luna_proto::PermissionScope::Workspace,
                expires_at_ms: i64::MAX,
                supports_task_allowance: false,
                details: None,
            }),
            input: None,
        }
    }

    fn pending_command(id: &str, payload: SessionCommandPayload) -> SessionCommandEntry {
        SessionCommandEntry {
            id: id.into(),
            payload,
            issued_by: "claimed-local-ui".into(),
            issued_at: 1,
            based_on: None,
            expires_at: None,
            status: SessionCommandStatus::Pending,
            resolution: None,
        }
    }

    #[tokio::test]
    async fn restarted_luna_approval_rejects_generic_answers_without_resolving_input() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(root.path().join("store")).unwrap());
        let doc = SessionDoc::init("chat-a").unwrap();
        for (request_id, kind) in [
            (
                "luna-request",
                luna_proto::ApprovalActionKind::LunaCrossTask,
            ),
            ("provider-request", luna_proto::ApprovalActionKind::Command),
        ] {
            doc.push_message(&SessionMessageEntry {
                id: format!("message-{request_id}"),
                role: MessageRole::Assistant,
                parts: vec![MessagePart::Input {
                    id: request_id.into(),
                    request_id: request_id.into(),
                    questions: vec![approval_question(kind)],
                    resolved: false,
                }],
                created_at: 1,
                device_id: "device-test".into(),
                status: Some(MessageStatus::Aborted),
                continuation_of: None,
            })
            .unwrap();
        }
        store
            .save_snapshot("chat-a", &doc.export_snapshot().unwrap())
            .unwrap();
        let host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "device-test".into(),
                default_harness: HarnessId::Codex,
                edge: None,
            },
        );
        let handle = host.open("chat-a").unwrap();
        let sessions = SessionsEngine::new(
            "device-test".into(),
            Arc::new(RunJournal::open(root.path().join("journals")).unwrap()),
            Arc::new(HarnessRegistry::new()),
            store,
        );
        assert!(!recoverable_provider_approval(&handle.doc, "luna-request"));
        assert!(recoverable_provider_approval(
            &handle.doc,
            "provider-request"
        ));

        for payload in [
            SessionCommandPayload::RespondApproval {
                request_id: "luna-request".into(),
                decision: luna_proto::ApprovalDecision::AllowOnce,
            },
            SessionCommandPayload::RespondInput {
                request_id: "luna-request".into(),
                answers: vec![UserInputAnswer {
                    question_id: "question".into(),
                    labels: vec!["Allow once".into()],
                }],
            },
        ] {
            let entry = pending_command("forged-luna-answer", payload);
            let (status, _) = host.execute(&sessions, &handle, &entry).await.unwrap();
            assert_eq!(status, SessionCommandStatus::Rejected);
            assert!(matches!(
                &handle
                    .doc
                    .read_entries()
                    .unwrap()
                    .iter()
                    .find(|entry| entry.id == "message-luna-request")
                    .unwrap()
                    .parts[0],
                MessagePart::Input {
                    resolved: false,
                    ..
                }
            ));
        }

        let entry = pending_command(
            "provider-answer",
            SessionCommandPayload::RespondApproval {
                request_id: "provider-request".into(),
                decision: luna_proto::ApprovalDecision::AllowOnce,
            },
        );
        let (status, detail) = host.execute(&sessions, &handle, &entry).await.unwrap();
        assert_eq!(status, SessionCommandStatus::Applied);
        assert!(detail.unwrap().contains("denied safely"));
        assert!(matches!(
            &handle
                .doc
                .read_entries()
                .unwrap()
                .iter()
                .find(|entry| entry.id == "message-provider-request")
                .unwrap()
                .parts[0],
            MessagePart::Input { resolved: true, .. }
        ));
        host.shutdown_workers().await;
    }
}

/// The resumed-turn prompt for answers to a question whose run died: each
/// answer paired with its question text so the reattached conversation reads
/// naturally. Pure.
pub fn respond_input_prompt(
    questions: &[UserInputQuestion],
    answers: &[UserInputAnswer],
) -> String {
    let mut lines = vec!["Answering your earlier question:".to_string()];
    for answer in answers {
        let picked = answer.labels.join(", ");
        let question = questions
            .iter()
            .find(|q| q.id == answer.question_id)
            .map(|q| q.question.trim())
            .filter(|q| !q.is_empty());
        match question {
            Some(question) => lines.push(format!("{question} — {picked}")),
            None => lines.push(picked),
        }
    }
    lines.join("\n")
}

/// Percent-encode one URL path segment of a sidecar part id. PART_RE's
/// alphabet includes `#` and `:` — legal in R2 keys and doc refs, but a raw
/// `#` in a URL is a fragment delimiter (the request would silently hit the
/// truncated key, colliding parts). The Worker decodes before validating.
fn encode_part_segment(part_id: &str) -> String {
    let mut out = String::with_capacity(part_id.len());
    for byte in part_id.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod transfer_progress_tests {
    use super::{DocHost, DocHostConfig, TransferProgressGuard};
    use std::sync::Arc;

    fn host() -> (tempfile::TempDir, DocHost) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(luna_sync::DocsStore::open(dir.path()).expect("store opens"));
        let host = DocHost::new(
            store,
            DocHostConfig {
                device_id: "dev-test".into(),
                default_harness: luna_proto::HarnessId::Mock,
                edge: None,
            },
        );
        (dir, host)
    }

    #[test]
    fn set_upserts_by_upload_id_and_clear_retires_only_its_entry() {
        let (_dir, host) = host();
        let rx = host.watch_transfers();
        assert!(rx.borrow().is_empty());

        host.transfer_progress_set("u1", "a.png", 0, 1_000);
        host.transfer_progress_set("u2", "b.png", 0, 400);
        host.transfer_progress_set("u1", "a.png", 300, 1_000);
        let snapshot = rx.borrow().clone();
        assert_eq!(snapshot.len(), 2, "upsert must not duplicate u1");
        let u1 = snapshot.iter().find(|t| t.upload_id == "u1").unwrap();
        assert_eq!((u1.done, u1.total), (300, 1_000));

        host.transfer_progress_clear("u1");
        let snapshot = rx.borrow().clone();
        assert_eq!(snapshot.len(), 1, "u2 must survive u1's retirement");
        assert_eq!(snapshot[0].upload_id, "u2");
    }

    #[test]
    fn guard_retires_the_entry_on_every_exit_path() {
        let (_dir, host) = host();
        let rx = host.watch_transfers();
        host.transfer_progress_set("u1", "a.png", 0, 9);
        {
            let _guard = TransferProgressGuard {
                host: &host,
                upload_id: "u1",
            };
            assert_eq!(rx.borrow().len(), 1);
            // An early return / error propagation drops the guard here.
        }
        assert!(
            rx.borrow().is_empty(),
            "a failed attempt must not leave a phantom ring behind"
        );
    }

    #[test]
    fn late_subscriber_sees_current_set_first() {
        let (_dir, host) = host();
        host.transfer_progress_set("u1", "a.png", 750, 1_000);
        // watch_stream's contract: current value first, then changes — a UI
        // attaching mid-transfer must render the ring immediately.
        let rx = host.watch_transfers();
        assert_eq!(rx.borrow().len(), 1);
        assert_eq!(rx.borrow()[0].done, 750);
    }
}

#[cfg(test)]
mod source_context_tests {
    use super::{DocHost, DocHostConfig};
    use std::process::Command;
    use std::sync::Arc;

    fn git(repo: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    #[tokio::test]
    async fn capture_source_context_reads_the_dispatch_checkout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "feature/captured"]);
        git(&repo, &["config", "user.name", "Luna Test"]);
        git(&repo, &["config", "user.email", "luna@example.com"]);
        std::fs::write(repo.join("README.md"), "capture\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "capture"]);

        let store =
            Arc::new(luna_sync::DocsStore::open(dir.path().join("docs")).expect("store opens"));
        let host = DocHost::new(
            store,
            DocHostConfig {
                device_id: "device-a".into(),
                default_harness: luna_proto::HarnessId::Mock,
                edge: None,
            },
        );
        host.set_repos(crate::repos::Repos::new(
            &dir.path().join("data"),
            "device-a",
        ));

        let context = host
            .capture_source_context(repo.to_str().unwrap())
            .await
            .expect("source context");
        assert_eq!(context.branch, "feature/captured");
        assert_eq!(context.cwd, repo.to_string_lossy());
        assert_eq!(
            context.repo_root,
            repo.canonicalize().unwrap().to_string_lossy()
        );
        assert!(context.head_sha.is_some());
        assert!(!context.checkout_id.is_empty());
    }
}

#[cfg(test)]
mod degrade_grace_tests {
    use super::{DEGRADE_GRACE, DegradeGrace, GraceKey};
    use std::time::{Duration, Instant};

    #[test]
    fn blips_shorter_than_the_grace_never_report() {
        let mut g = DegradeGrace::default();
        let t0 = Instant::now();
        // A 300ms room join: degraded at t0, healthy again shortly after.
        assert!(!g.degraded(GraceKey::Chat("c1"), true, t0));
        assert!(!g.degraded(GraceKey::Chat("c1"), true, t0 + Duration::from_millis(300)));
        assert!(!g.degraded(GraceKey::Chat("c1"), false, t0 + Duration::from_millis(600)));
        // The recovery cleared the timer — a fresh blip starts from zero.
        assert!(!g.degraded(GraceKey::Chat("c1"), true, t0 + Duration::from_secs(10)));
    }

    #[test]
    fn persistent_degradation_reports_after_the_grace_and_clears_instantly() {
        let mut g = DegradeGrace::default();
        let t0 = Instant::now();
        assert!(!g.degraded(GraceKey::Registry, true, t0));
        assert!(!g.degraded(GraceKey::Registry, true, t0 + DEGRADE_GRACE / 2));
        assert!(g.degraded(GraceKey::Registry, true, t0 + DEGRADE_GRACE));
        assert!(g.degraded(GraceKey::Registry, true, t0 + DEGRADE_GRACE * 3));
        // Hide-fast: one healthy sample reports Connected immediately.
        assert!(!g.degraded(GraceKey::Registry, false, t0 + DEGRADE_GRACE * 4));
    }

    #[test]
    fn sources_are_independent_and_closed_chats_are_dropped() {
        let mut g = DegradeGrace::default();
        let t0 = Instant::now();
        assert!(!g.degraded(GraceKey::Chat("gone"), true, t0));
        assert!(!g.degraded(GraceKey::OsPath, true, t0));
        // The chat's doc closes; its timer must not leak.
        g.retain_chats(|id| id != "gone");
        assert!(g.chats.is_empty());
        // OsPath kept its own timer through the retain.
        assert!(g.degraded(GraceKey::OsPath, true, t0 + DEGRADE_GRACE));
    }
}

#[cfg(test)]
mod part_segment_tests {
    use super::encode_part_segment;

    #[test]
    fn hash_and_colon_are_escaped_unreserved_pass_through() {
        assert_eq!(encode_part_segment("m1#c1"), "m1%23c1");
        assert_eq!(encode_part_segment("tool:call_9"), "tool%3Acall_9");
        assert_eq!(encode_part_segment("plain-id_0.diff~"), "plain-id_0.diff~");
    }
}

#[cfg(test)]
mod queued_message_prompt_tests {
    use super::{ATTACHMENT_ONLY_PROMPT, ATTACHMENT_PROMPT_HEADER, queued_message_prompt};

    #[test]
    fn dispatch_adds_the_attachment_transport_to_visible_queue_text() {
        let paths = vec!["/tmp/image.png".to_string()];
        assert_eq!(
            queued_message_prompt("inspect this", &paths),
            format!("inspect this\n\n{ATTACHMENT_PROMPT_HEADER}\n- /tmp/image.png")
        );
    }

    #[test]
    fn legacy_expanded_rows_are_not_expanded_twice() {
        let paths = vec!["/tmp/image.png".to_string()];
        let legacy = format!("inspect this\n\n{ATTACHMENT_PROMPT_HEADER}\n- /tmp/image.png");
        assert_eq!(queued_message_prompt(&legacy, &paths), legacy);
    }

    #[test]
    fn attachment_only_rows_get_a_non_empty_prompt_body() {
        let paths = vec!["/tmp/image.png".to_string()];
        assert_eq!(
            queued_message_prompt("", &paths),
            format!("{ATTACHMENT_ONLY_PROMPT}\n\n{ATTACHMENT_PROMPT_HEADER}\n- /tmp/image.png")
        );
    }
}

/// Per-chat background task: reacts to doc changes (local commits and remote imports)
/// by re-publishing the transcript watch, draining commands, and debouncing snapshots.
/// Holds only a weak handle so a dropped host tears the task down.
async fn chat_task(host: DocHost, weak: Weak<ChatDocHandle>, mut changed_rx: watch::Receiver<u64>) {
    // Initial pass: the snapshot may already carry pending commands. The
    // mirror stays lazy — it materializes on the first watch attach.
    {
        let Some(handle) = weak.upgrade() else { return };
        host.drain_commands(&handle).await;
        host.drain_queue(&handle).await;
    }
    let mut save_deadline: Option<tokio::time::Instant> = None;
    loop {
        let sleep_until = save_deadline.unwrap_or_else(tokio::time::Instant::now);
        tokio::select! {
            changed = changed_rx.changed() => {
                if changed.is_err() {
                    break; // doc handle (and its change sender) is gone
                }
                let Some(handle) = weak.upgrade() else { break };
                handle.publish_messages_if_watched();
                handle.publish_queue();
                host.drain_commands(&handle).await;
                host.drain_queue(&handle).await;
                if save_deadline.is_none() {
                    save_deadline = Some(
                        tokio::time::Instant::now()
                            + std::time::Duration::from_millis(SNAPSHOT_DEBOUNCE_MS),
                    );
                }
            }
            _ = tokio::time::sleep_until(sleep_until), if save_deadline.is_some() => {
                save_deadline = None;
                let Some(handle) = weak.upgrade() else { break };
                host.save_snapshot(&handle);
                // chat2 host duties ride the same quiesce tick (C3):
                // threshold checkpoints + the tail sidecar publish.
                host.chat2_maintenance(&handle).await;
                // Post-quiesce eviction pass: sizes just refreshed.
                host.evict_over_budget();
            }
        }
    }
}

#[cfg(test)]
mod publication_eviction_tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn lru_eviction_replays_unacknowledged_updates_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(dir.path()).unwrap());
        let host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "writer".into(),
                default_harness: HarnessId::Codex,
                edge: Some(EdgeConfig::with_static_token("http://127.0.0.1:1", "test")),
            },
        );
        let handle = host.open("evicted").unwrap();
        handle
            .doc
            .doc()
            .get_text("body")
            .insert(0, "unacknowledged cleanup")
            .unwrap();
        handle.doc.doc().commit();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while lock(&handle.chat2).is_none() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(lock(&handle.chat2).as_ref().unwrap().stats().pending_pushes > 0);
        for i in 0..WARM_DOC_CAP {
            host.open(&format!("other-{i}")).unwrap();
        }
        handle
            .last_access
            .store(now_ms() - 2 * EVICT_MIN_IDLE_MS, Ordering::Relaxed);
        assert!(
            !host.pinned(&handle),
            "durably queued ops need not retain the whole doc in memory"
        );
        host.evict_over_budget();
        assert!(!lock(&host.inner.handles).contains_key("evicted"));
        drop(handle);
        let before = store.pending_chat_updates("evicted").unwrap();
        let reopened = host.open("evicted").unwrap();
        assert_eq!(
            reopened.doc.doc().get_text("body").to_string(),
            "unacknowledged cleanup"
        );
        assert_eq!(store.pending_chat_updates("evicted").unwrap(), before);
        drop(reopened);
        host.shutdown_workers().await;
    }

    #[tokio::test]
    async fn retired_fat_handle_commits_queue_rows_into_the_thin_lineage() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(dir.path()).unwrap());
        let fat = SessionDoc::init("retired-queue").unwrap();
        store
            .save_snapshot("retired-queue", &fat.export_snapshot().unwrap())
            .unwrap();
        let host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "writer".into(),
                default_harness: HarnessId::Mock,
                edge: None,
            },
        );
        let handle = host.open("retired-queue").unwrap();
        let _watch = handle.watch_queue();

        let thin = SessionDoc::init("retired-queue").unwrap();
        store
            .save_snapshot_with_cursor(
                "retired-queue",
                &thin.export_snapshot().unwrap(),
                7,
                crate::chat2_host::CHAT2_DOC_EPOCH,
            )
            .unwrap();
        handle.retired.store(true, Ordering::Release);

        assert!(
            host.queue_message_with_id_and_execution(
                "retired-queue",
                "thin-queue-row",
                "survive retirement",
                Vec::new(),
                true,
                luna_proto::FollowUpBehavior::Queue,
                None,
            )
            .unwrap()
        );
        let (snapshot, cursor, epoch) = store
            .load_snapshot_with_cursor("retired-queue")
            .unwrap()
            .unwrap();
        assert_eq!(cursor, 7);
        assert_eq!(epoch, crate::chat2_host::CHAT2_DOC_EPOCH);
        let raw = loro::LoroDoc::new();
        raw.import(&snapshot).unwrap();
        let stored = SessionDoc::from_doc(raw);
        assert_eq!(
            stored
                .read_queue()
                .unwrap()
                .into_iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec!["thin-queue-row"]
        );
        drop(handle);
        host.shutdown_workers().await;
    }
}

#[cfg(test)]
mod approved_queue_projection_tests {
    use super::*;
    use crate::chat2_host::EngineChatSink;
    use luna_sync::chat_client::{ChatDocSink, RowImportOutcome};

    fn fixture() -> (
        tempfile::TempDir,
        Arc<DocsStore>,
        DocHost,
        Arc<ChatDocHandle>,
    ) {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(root.path()).unwrap());
        let doc = SessionDoc::init("target").unwrap();
        store
            .save_snapshot("target", &doc.export_snapshot().unwrap())
            .unwrap();
        store.initialize_chat_outbox("target", &[]).unwrap();
        let host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "synthetic-device".into(),
                default_harness: HarnessId::Mock,
                edge: None,
            },
        );
        let handle = host.open("target").unwrap();
        (root, store, host, handle)
    }

    fn begin(host: &DocHost, command_id: &str) {
        let now = now_ms();
        let pending = CommandReceipt {
            command_id: command_id.into(),
            task_id: "target".into(),
            intent: "queueMessage".into(),
            payload_hash: "synthetic-approved-send".into(),
            status: CommandReceiptStatus::Pending,
            snapshot: host.task_command_snapshot("target").unwrap().snapshot,
            event_sequence: None,
            actor_id: "luna-tool:source".into(),
            client_id: "luna-cross-task".into(),
            client_session_id: "source".into(),
            execution_environment: Some("synthetic-device".into()),
            correlation_id: None,
            reason: None,
            recovery_action: None,
            accepted_at_ms: now,
            committed_at_ms: None,
            observed_at_ms: now,
        };
        assert_eq!(
            host.begin_task_command(pending, None, None, None)
                .unwrap()
                .kind,
            luna_sync::TaskCommandBeginKind::Accepted
        );
    }

    fn queue(doc: &SessionDoc) -> Result<((), bool), EngineError> {
        doc.push_queued(&QueuedMessage {
            input_items: Vec::new(),
            id: "approved-send".into(),
            text: "synthetic queued follow-up".into(),
            attachments: Vec::new(),
            hold_for_turn_end: true,
            follow_up_behavior: luna_proto::FollowUpBehavior::Queue,
            hold_reason: None,
            issued_by: "synthetic-device".into(),
            issued_at: now_ms(),
            edited_at: None,
            revision: 1,
            execution: None,
            status: QueueMessageStatus::Held,
            delivery_gate: None,
        })?;
        Ok(((), true))
    }

    fn stored_queue(store: &DocsStore) -> Vec<QueuedMessage> {
        SessionDoc::from_snapshot(&store.load_snapshot("target").unwrap().unwrap())
            .unwrap()
            .read_queue()
            .unwrap()
    }

    #[tokio::test]
    async fn approved_send_postcommit_failure_blocks_stale_saves_and_recovers_on_restart() {
        let (_root, store, host, handle) = fixture();
        begin(&host, "approved-send");
        let result = host.commit_approved_queue_projection_after_commit(
            &handle,
            "approved-send",
            queue,
            || Err(EngineError::Other("synthetic install interruption".into())),
        );
        assert!(matches!(result, Err(TaskProjectionError::Commit(_))));
        let receipt = store
            .task_command_receipt("approved-send")
            .unwrap()
            .unwrap();
        assert_eq!(receipt.status, CommandReceiptStatus::Applied);
        assert_eq!(stored_queue(&store).len(), 1);
        assert_eq!(store.pending_chat_updates("target").unwrap().len(), 1);
        assert!(handle.doc.read_queue().unwrap().is_empty());
        let durable = store.load_snapshot_with_cursor("target").unwrap().unwrap();
        assert!(host.save_snapshot_checked(&handle).is_err());
        assert!(host.flush_all_checked().is_err());
        host.flush_all();
        let sink = EngineChatSink::new(&handle.doc, store.clone(), "target")
            .with_handle(Arc::downgrade(&handle));
        assert_eq!(
            sink.apply_row(&[0], 1),
            RowImportOutcome::PendingDependencies
        );
        assert!(sink.apply_checkpoint(&[0], 1).is_err());
        assert_eq!(
            store.load_snapshot_with_cursor("target").unwrap().unwrap(),
            durable
        );
        assert!(host.open("target").is_err());
        drop(sink);
        drop(handle);
        drop(host);
        let restarted = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "synthetic-device".into(),
                default_harness: HarnessId::Mock,
                edge: None,
            },
        );
        let reopened = restarted.open("target").unwrap();
        assert_eq!(reopened.doc.read_queue().unwrap().len(), 1);
        assert_eq!(stored_queue(&store).len(), 1);
        assert_eq!(store.pending_chat_updates("target").unwrap().len(), 1);
        assert_eq!(
            store
                .task_command_receipt("approved-send")
                .unwrap()
                .unwrap()
                .status,
            CommandReceiptStatus::Applied
        );
    }

    #[tokio::test]
    async fn approved_send_sql_failure_is_precommit_and_never_returns_queued() {
        let (root, store, host, handle) = fixture();
        begin(&host, "approved-send");
        let db = rusqlite::Connection::open(root.path().join("docs.sqlite3")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER fail_approved_queue BEFORE UPDATE ON snapshots
             WHEN OLD.doc_id='target'
             BEGIN SELECT RAISE(ABORT, 'synthetic approved queue failure'); END;",
        )
        .unwrap();
        let result = host.commit_approved_queue_projection_after_commit(
            &handle,
            "approved-send",
            queue,
            || panic!("precommit failure reached install"),
        );
        assert!(matches!(result, Err(TaskProjectionError::Mutation(_))));
        assert_eq!(
            store
                .task_command_receipt("approved-send")
                .unwrap()
                .unwrap()
                .status,
            CommandReceiptStatus::Pending
        );
        assert!(stored_queue(&store).is_empty());
        assert!(store.pending_chat_updates("target").unwrap().is_empty());
        assert!(
            !handle
                .durable_projection_uninstalled
                .load(Ordering::Acquire)
        );
        db.execute_batch("DROP TRIGGER fail_approved_queue")
            .unwrap();
        host.save_snapshot_checked(&handle).unwrap();
    }
}

#[cfg(test)]
mod unwired_interrupt_projection_tests {
    use super::*;
    use crate::chat2_host::EngineChatSink;
    use luna_sync::chat_client::{ChatDocSink, RowImportOutcome};

    fn fixture() -> (
        tempfile::TempDir,
        Arc<DocsStore>,
        DocHost,
        Arc<ChatDocHandle>,
    ) {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(root.path()).unwrap());
        let doc = SessionDoc::init("target").unwrap();
        store
            .save_snapshot("target", &doc.export_snapshot().unwrap())
            .unwrap();
        store.initialize_chat_outbox("target", &[]).unwrap();
        let host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "synthetic-device".into(),
                default_harness: HarnessId::Mock,
                edge: None,
            },
        );
        let handle = host.open("target").unwrap();
        (root, store, host, handle)
    }

    fn stored_commands(store: &DocsStore) -> Vec<SessionCommandEntry> {
        SessionDoc::from_snapshot(&store.load_snapshot("target").unwrap().unwrap())
            .unwrap()
            .read_commands()
            .unwrap()
    }

    fn remote_row(store: &DocsStore) -> (Vec<u8>, Vec<u8>) {
        let source =
            SessionDoc::from_snapshot(&store.load_snapshot("target").unwrap().unwrap()).unwrap();
        let frontier = source.doc().oplog_vv();
        source.doc().get_text("body").insert(0, "remote").unwrap();
        source.doc().commit();
        (
            source
                .doc()
                .export(loro::ExportMode::updates(&frontier))
                .unwrap(),
            source.export_snapshot().unwrap(),
        )
    }

    fn remote_sink(store: Arc<DocsStore>, handle: &Arc<ChatDocHandle>) -> Arc<EngineChatSink> {
        Arc::new(
            EngineChatSink::new(&handle.doc, store, "target").with_handle(Arc::downgrade(handle)),
        )
    }

    fn stored_stop_and_remote(store: &DocsStore, command_id: &str) -> u64 {
        let (snapshot, cursor, _) = store.load_snapshot_with_cursor("target").unwrap().unwrap();
        let doc = SessionDoc::from_snapshot(&snapshot).unwrap();
        let commands = doc.read_commands().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].id, command_id);
        assert_eq!(doc.doc().get_text("body").to_string(), "remote");
        cursor
    }

    #[tokio::test]
    async fn checked_shutdown_retains_open_doc_after_synthetic_sql_failure() {
        let (root, _store, host, _handle) = fixture();
        let db = rusqlite::Connection::open(root.path().join("docs.sqlite3")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER fail_shutdown_snapshot BEFORE UPDATE ON snapshots
             WHEN OLD.doc_id='target'
             BEGIN SELECT RAISE(ABORT, 'synthetic shutdown failure'); END;",
        )
        .unwrap();
        assert!(host.shutdown_workers_checked().await.is_err());
        assert!(lock(&host.inner.handles).contains_key("target"));
        db.execute_batch("DROP TRIGGER fail_shutdown_snapshot;")
            .unwrap();
        host.shutdown_workers_checked().await.unwrap();
        assert!(!lock(&host.inner.handles).contains_key("target"));
    }

    #[tokio::test]
    async fn unwired_interrupt_persists_one_exact_id_and_rejects_payload_collision() {
        let (_root, store, host, handle) = fixture();
        assert!(
            host.persist_unwired_interrupt_with_id("target", "stop-a")
                .unwrap()
        );
        let snapshot_after_first = store.load_snapshot("target").unwrap().unwrap();
        let outbox_after_first = store.pending_chat_updates("target").unwrap();
        assert!(
            !host
                .persist_unwired_interrupt_with_id("target", "stop-a")
                .unwrap()
        );
        assert_eq!(
            store.load_snapshot("target").unwrap().unwrap(),
            snapshot_after_first
        );
        assert_eq!(
            store.pending_chat_updates("target").unwrap(),
            outbox_after_first
        );
        let commands = stored_commands(&store);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].id, "stop-a");
        assert_eq!(commands[0].payload, SessionCommandPayload::Interrupt {});
        assert_eq!(store.pending_chat_updates("target").unwrap().len(), 1);

        handle
            .doc
            .queue_command(&SessionCommandEntry {
                id: "occupied-id".into(),
                payload: SessionCommandPayload::Steer {
                    attachments: Vec::new(),
                    input_items: Vec::new(),
                    prompt: "synthetic only".into(),
                    message_id: None,
                },
                issued_by: "synthetic-device".into(),
                issued_at: 1,
                based_on: None,
                expires_at: None,
                status: SessionCommandStatus::Pending,
                resolution: None,
            })
            .unwrap();
        assert!(
            host.persist_unwired_interrupt_with_id("target", "occupied-id")
                .is_err()
        );
        assert_eq!(
            handle
                .doc
                .read_commands()
                .unwrap()
                .iter()
                .filter(|entry| entry.id == "occupied-id")
                .count(),
            1
        );
        assert!(
            host.persist_unwired_interrupt_with_id("target", "")
                .is_err()
        );
    }

    #[tokio::test]
    async fn unwired_interrupt_storage_failure_publishes_no_row_or_outbox() {
        for trigger in [
            "CREATE TRIGGER fail_stop_snapshot BEFORE UPDATE ON snapshots
             WHEN OLD.doc_id='target'
             BEGIN SELECT RAISE(ABORT, 'synthetic stop snapshot failure'); END;",
            "CREATE TRIGGER fail_stop_outbox BEFORE INSERT ON chat_outbox
             WHEN NEW.doc_id='target'
             BEGIN SELECT RAISE(ABORT, 'synthetic stop outbox failure'); END;",
        ] {
            let (root, store, host, handle) = fixture();
            let db = rusqlite::Connection::open(root.path().join("docs.sqlite3")).unwrap();
            db.execute_batch(trigger).unwrap();
            assert!(
                host.persist_unwired_interrupt_with_id("target", "stop-failed")
                    .is_err()
            );
            assert!(handle.doc.read_commands().unwrap().is_empty());
            assert!(stored_commands(&store).is_empty());
            assert!(store.pending_chat_updates("target").unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn unwired_interrupt_post_commit_failure_survives_restart_once() {
        let (_root, store, host, handle) = fixture();
        assert!(
            host.persist_unwired_interrupt_with_id_after_commit("target", "stop-uncertain", || {
                Err(EngineError::Other(
                    "synthetic failure before live import".into(),
                ))
            })
            .is_err()
        );
        assert!(handle.doc.read_commands().unwrap().is_empty());
        assert_eq!(stored_commands(&store).len(), 1);
        assert_eq!(store.pending_chat_updates("target").unwrap().len(), 1);
        host.save_snapshot(&handle);
        assert_eq!(stored_commands(&store).len(), 1);
        assert!(
            host.open("target").is_err(),
            "stale handle must fail closed"
        );
        drop(handle);
        drop(host);

        let restarted = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "synthetic-device".into(),
                default_harness: HarnessId::Mock,
                edge: None,
            },
        );
        let reopened = restarted.open("target").unwrap();
        let commands = reopened.doc.read_commands().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].id, "stop-uncertain");
        assert!(
            !restarted
                .persist_unwired_interrupt_with_id("target", "stop-uncertain")
                .unwrap()
        );
        assert_eq!(stored_commands(&store).len(), 1);
        assert_eq!(store.pending_chat_updates("target").unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unwired_interrupt_commit_blocks_stale_snapshot_saver() {
        let (_root, store, host, handle) = fixture();
        let saver_host = host.clone();
        let saver_handle = handle.clone();
        let attempted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let attempted_for_saver = attempted.clone();
        let finished_for_saver = finished.clone();
        let (start_tx, start_rx) = std::sync::mpsc::channel();
        let saver = std::thread::spawn(move || {
            start_rx.recv().unwrap();
            attempted_for_saver.store(true, Ordering::Release);
            saver_host.save_snapshot(&saver_handle);
            finished_for_saver.store(true, Ordering::Release);
        });
        assert!(
            host.persist_unwired_interrupt_with_id_after_commit("target", "stop-race", || {
                start_tx.send(()).unwrap();
                while !attempted.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                std::thread::sleep(std::time::Duration::from_millis(30));
                assert!(!finished.load(Ordering::Acquire));
                Ok(())
            })
            .unwrap()
        );
        saver.join().unwrap();
        assert!(finished.load(Ordering::Acquire));
        let commands = stored_commands(&store);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].id, "stop-race");
        assert_eq!(store.pending_chat_updates("target").unwrap().len(), 1);
    }

    #[tokio::test]
    async fn remote_row_before_projection_survives_with_cursor_and_stop() {
        let (_root, store, host, handle) = fixture();
        let (row, _) = remote_row(&store);
        let sink = remote_sink(store.clone(), &handle);
        assert_eq!(sink.apply_row(&row, 1), RowImportOutcome::Applied);
        assert!(
            host.persist_unwired_interrupt_with_id("target", "stop-after-remote")
                .unwrap()
        );
        assert_eq!(stored_stop_and_remote(&store, "stop-after-remote"), 1);
        assert_eq!(store.pending_chat_updates("target").unwrap().len(), 1);
    }

    #[tokio::test]
    async fn handle_backed_checkpoint_and_replay_keep_cursor_with_stop() {
        let (_root, store, host, handle) = fixture();
        let (row, checkpoint) = remote_row(&store);
        let sink = remote_sink(store.clone(), &handle);
        sink.apply_checkpoint(&checkpoint, 1).unwrap();
        assert_eq!(sink.apply_replay_row(&row, 1), RowImportOutcome::Applied);
        assert!(
            host.persist_unwired_interrupt_with_id("target", "stop-after-checkpoint")
                .unwrap()
        );
        assert_eq!(stored_stop_and_remote(&store, "stop-after-checkpoint"), 1);
        assert_eq!(store.pending_chat_updates("target").unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn projection_blocks_remote_row_until_stop_is_installed() {
        let (_root, store, host, handle) = fixture();
        let (row, _) = remote_row(&store);
        let sink = remote_sink(store.clone(), &handle);
        let attempted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let attempted_for_row = attempted.clone();
        let finished_for_row = finished.clone();
        let (start_tx, start_rx) = std::sync::mpsc::channel();
        let remote = std::thread::spawn(move || {
            start_rx.recv().unwrap();
            attempted_for_row.store(true, Ordering::Release);
            let outcome = sink.apply_row(&row, 1);
            finished_for_row.store(true, Ordering::Release);
            outcome
        });
        assert!(
            host.persist_unwired_interrupt_with_id_after_commit(
                "target",
                "stop-before-remote",
                || {
                    start_tx.send(()).unwrap();
                    while !attempted.load(Ordering::Acquire) {
                        std::thread::yield_now();
                    }
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    assert!(!finished.load(Ordering::Acquire));
                    Ok(())
                }
            )
            .unwrap()
        );
        assert_eq!(remote.join().unwrap(), RowImportOutcome::Applied);
        assert_eq!(stored_stop_and_remote(&store, "stop-before-remote"), 1);
        assert_eq!(store.pending_chat_updates("target").unwrap().len(), 1);
        drop(handle);
        drop(host);
        let restarted = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "synthetic-device".into(),
                default_harness: HarnessId::Mock,
                edge: None,
            },
        );
        let reopened = restarted.open("target").unwrap();
        assert_eq!(reopened.doc.read_commands().unwrap().len(), 1);
        assert_eq!(reopened.doc.doc().get_text("body").to_string(), "remote");
    }

    #[tokio::test]
    async fn poisoned_projection_defers_remote_rows_and_checkpoint_without_cursor_change() {
        let (_root, store, host, handle) = fixture();
        let (row, checkpoint) = remote_row(&store);
        let sink = remote_sink(store.clone(), &handle);
        assert!(
            host.persist_unwired_interrupt_with_id_after_commit("target", "stop-poisoned", || {
                Err(EngineError::Other("synthetic failure before import".into()))
            })
            .is_err()
        );
        let before = store.load_snapshot_with_cursor("target").unwrap().unwrap();
        let outbox_before = store.pending_chat_updates("target").unwrap();
        assert_eq!(
            sink.apply_row(&row, 1),
            RowImportOutcome::PendingDependencies
        );
        assert_eq!(
            sink.apply_replay_row(&row, 1),
            RowImportOutcome::PendingDependencies
        );
        assert!(sink.apply_checkpoint(&checkpoint, 1).is_err());
        assert_eq!(
            store.load_snapshot_with_cursor("target").unwrap().unwrap(),
            before
        );
        assert_eq!(store.pending_chat_updates("target").unwrap(), outbox_before);
        assert!(handle.doc.read_commands().unwrap().is_empty());
        assert_eq!(handle.doc.doc().get_text("body").to_string(), "");
        drop(handle);
        drop(host);
        let restarted = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "synthetic-device".into(),
                default_harness: HarnessId::Mock,
                edge: None,
            },
        );
        let reopened = restarted.open("target").unwrap();
        let commands = reopened.doc.read_commands().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].id, "stop-poisoned");
        assert_eq!(
            store
                .load_snapshot_with_cursor("target")
                .unwrap()
                .unwrap()
                .1,
            0
        );
    }

    #[tokio::test]
    async fn remote_cursor_sql_failure_keeps_committed_stop_and_old_cursor() {
        let (root, store, host, handle) = fixture();
        let (row, _) = remote_row(&store);
        assert!(
            host.persist_unwired_interrupt_with_id("target", "stop-cursor-failure")
                .unwrap()
        );
        let before = store.load_snapshot_with_cursor("target").unwrap().unwrap();
        let outbox_before = store.pending_chat_updates("target").unwrap();
        let db = rusqlite::Connection::open(root.path().join("docs.sqlite3")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER fail_remote_cursor BEFORE UPDATE ON snapshots
             WHEN OLD.doc_id='target'
             BEGIN SELECT RAISE(ABORT, 'synthetic cursor failure'); END;",
        )
        .unwrap();
        let sink = remote_sink(store.clone(), &handle);
        assert_eq!(sink.apply_row(&row, 1), RowImportOutcome::Applied);
        assert_eq!(
            store.load_snapshot_with_cursor("target").unwrap().unwrap(),
            before
        );
        assert_eq!(store.pending_chat_updates("target").unwrap(), outbox_before);
        assert_eq!(stored_commands(&store)[0].id, "stop-cursor-failure");
        db.execute_batch("DROP TRIGGER fail_remote_cursor").unwrap();
    }
}
