//! ZI-02 E2E: a permanently local profile keeps current registry rows without
//! an ever-growing ack queue, while a sync-capable profile keeps every offline
//! intent through restart and reconnect
//! (docs/pending-tasks/zeron-upstream-improvements/zi-02/README.md).
//!
//! Real `EngineCore`s on disposable data dirs, real SQLite persistence, the
//! in-process registry server (`luna_sync::registry::mock_server`, the edge's
//! merge fn and JSON protocol) and real `RegistryClient`s. Oracles are
//! independent of the code under test: expected rows come from the writes
//! each test issues or from a literal fixture, and persisted snapshots are
//! read straight from `docs.sqlite3` with rusqlite and decoded as plain JSON,
//! never through `RegistryDoc`. Canonical state is hashed from a fixed-field
//! projection (SHA-256 over its JSON). Set `LUNA_ZI02_EVIDENCE_DIR` to retain
//! `registry-growth-e2e.json`, `registry-migration-e2e.json` and
//! `registry-reconnect-e2e.json` (aggregate counts, bytes, timings, hashes).

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::{SinkExt, StreamExt};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessId, HarnessRegistry};
use luna_proto::{
    Chat, CommandReceipt, CommandReceiptStatus, Session, SessionStatus, SettlementDisposition,
    Space, TaskCommandRead,
};
use luna_rpc::{RpcClient, methods};
use luna_sync::registry::mock_server::MockRegistryServer;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message as WsMessage;

const BASE_MS: i64 = 1_700_000_000_000;
const CHATS: usize = 8;
const SPACES: usize = 2;
/// The specified growth points for the permanently local profile.
const LOCAL_CHECKPOINTS: [usize; 2] = [25_000, 100_000];
/// The sync-capable baseline replays every queued op on every overlay read
/// (quadratic), so it is measured at smaller counts and extrapolated.
const REPLICA_CHECKPOINTS: [usize; 2] = [500, 2_000];
/// One write in this many goes through the guarded task-command RPC path.
const RPC_EVERY: usize = 100;
const ORG: &str = "zi02-org";
const USER: &str = "zi02-user";

// ── assembly and raw persistence ────────────────────────────────────────────

fn ts(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).expect("fixture time")
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn try_assemble(profile: EngineProfile) -> Result<EngineCore, impl std::fmt::Debug> {
    let root = profile.device_root().to_path_buf();
    let accounts = AgentAccountsConfig {
        data_dir: root.join("synthetic-provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    };
    accounts
        .disable_claude_keychain_fallback()
        .expect("seal synthetic Claude home");
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        profile,
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        accounts,
    )
}

fn assemble(profile: EngineProfile) -> EngineCore {
    try_assemble(profile).expect("assemble profile")
}

async fn shutdown(core: EngineCore) {
    core.shutdown().await;
    drop(core);
    tokio::task::yield_now().await;
}

/// Pin the device id so HLC device suffixes and row owners are known.
fn pin_device(data_dir: &Path, device: &str) {
    std::fs::create_dir_all(data_dir).expect("data dir");
    std::fs::write(data_dir.join("device-id"), device).expect("device id");
}

fn database(profile: &EngineProfile) -> PathBuf {
    profile.store_root().join("docs.sqlite3")
}

/// The persisted registry snapshot exactly as SQLite holds it, through a
/// separate connection and decoded as plain JSON.
fn raw_snapshot(db: &Path) -> (Vec<u8>, Value) {
    let conn = rusqlite::Connection::open(db).expect("open registry database");
    let bytes: Vec<u8> = conn
        .query_row(
            "SELECT bytes FROM snapshots WHERE doc_id = 'registry1'",
            [],
            |row| row.get(0),
        )
        .expect("registry snapshot row");
    let value = serde_json::from_slice(&bytes).expect("registry snapshot JSON");
    (bytes, value)
}

fn reject_registry_writes(db: &Path) {
    rusqlite::Connection::open(db)
        .expect("open registry database")
        .execute_batch(
            "CREATE TRIGGER zi02_reject_registry BEFORE UPDATE ON snapshots
             WHEN NEW.doc_id='registry1'
             BEGIN SELECT RAISE(ABORT, 'zi02 synthetic registry write failure'); END;",
        )
        .expect("install registry write failure");
}

fn accept_registry_writes(db: &Path) {
    rusqlite::Connection::open(db)
        .expect("open registry database")
        .execute_batch("DROP TRIGGER zi02_reject_registry")
        .expect("remove registry write failure");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueueStats {
    pending_batches: usize,
    pending_ops: usize,
    rows: usize,
    snapshot_bytes: usize,
}

fn queue_stats(bytes: &[u8], snapshot: &Value) -> QueueStats {
    let pending = snapshot["pending"].as_array().expect("pending array");
    QueueStats {
        pending_batches: pending.len(),
        pending_ops: pending
            .iter()
            .map(|batch| batch["ops"].as_array().map_or(0, Vec::len))
            .sum(),
        rows: snapshot["rows"].as_array().map_or(0, Vec::len),
        snapshot_bytes: bytes.len(),
    }
}

fn raw_row<'a>(snapshot: &'a Value, kind: &str, id: &str) -> &'a Value {
    snapshot["rows"]
        .as_array()
        .expect("rows array")
        .iter()
        .find(|row| row["kind"] == kind && row["id"] == id)
        .unwrap_or_else(|| panic!("persisted row {kind}/{id}"))
}

fn retain_evidence(name: &str, value: &Value) {
    let Ok(dir) = std::env::var("LUNA_ZI02_EVIDENCE_DIR") else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("evidence dir");
    std::fs::write(
        Path::new(&dir).join(name),
        serde_json::to_vec_pretty(value).expect("evidence JSON"),
    )
    .expect("write evidence");
}

fn millis(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ── canonical projection ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpaceView {
    id: String,
    name: Option<String>,
    removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatView {
    id: String,
    space_id: Option<String>,
    title: Option<String>,
    archived: bool,
    pinned: bool,
    settled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionView {
    chat_id: String,
    status: String,
    updated_at: i64,
    last_completed_turn: Option<String>,
}

/// The fields every test controls, live rows only, sorted by id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct Canonical {
    spaces: Vec<SpaceView>,
    chats: Vec<ChatView>,
    sessions: Vec<SessionView>,
}

impl Canonical {
    fn hash(&self) -> String {
        sha256_hex(&serde_json::to_vec(self).expect("canonical JSON"))
    }
}

/// Which rows a test owns (background writers may add others).
#[derive(Debug, Clone, Default)]
struct Ids {
    spaces: BTreeSet<String>,
    chats: BTreeSet<String>,
}

impl Ids {
    fn new(spaces: &[&str], chats: &[&str]) -> Self {
        Self {
            spaces: spaces.iter().map(|id| id.to_string()).collect(),
            chats: chats.iter().map(|id| id.to_string()).collect(),
        }
    }
}

fn status_name(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "idle",
        SessionStatus::Working => "working",
        SessionStatus::AwaitingApproval => "awaitingApproval",
        SessionStatus::AwaitingInput => "awaitingInput",
        SessionStatus::Errored => "errored",
    }
}

fn space_views(spaces: &[Space], ids: &Ids) -> Vec<SpaceView> {
    let mut views: Vec<SpaceView> = spaces
        .iter()
        .filter(|space| ids.spaces.contains(&space.id))
        .map(|space| SpaceView {
            id: space.id.clone(),
            name: space.name.clone(),
            removed: space.removed_at.is_some(),
        })
        .collect();
    views.sort_by(|a, b| a.id.cmp(&b.id));
    views
}

fn chat_views(chats: &[Chat], ids: &Ids) -> Vec<ChatView> {
    let mut views: Vec<ChatView> = chats
        .iter()
        .filter(|chat| ids.chats.contains(&chat.id))
        .map(|chat| ChatView {
            id: chat.id.clone(),
            space_id: chat.space_id.clone(),
            title: chat.title.clone(),
            archived: chat.archived,
            pinned: chat.pinned_order.is_some(),
            settled: chat
                .settlement_override
                .as_ref()
                .map(|over| over.disposition == SettlementDisposition::Settled),
        })
        .collect();
    views.sort_by(|a, b| a.id.cmp(&b.id));
    views
}

fn session_views(sessions: &[Session], ids: &Ids) -> Vec<SessionView> {
    let mut views: Vec<SessionView> = sessions
        .iter()
        .filter(|session| ids.chats.contains(&session.chat_id))
        .map(|session| SessionView {
            chat_id: session.chat_id.clone(),
            status: status_name(session.status).to_string(),
            updated_at: session.updated_at.timestamp_millis(),
            last_completed_turn: session.last_completed_turn.clone(),
        })
        .collect();
    views.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
    views
}

/// What the engine's public registry reads return.
fn public_view(core: &EngineCore, ids: &Ids) -> Canonical {
    Canonical {
        spaces: space_views(&core.workspace.read_spaces().expect("read spaces"), ids),
        chats: chat_views(&core.workspace.read_chats().expect("read chats"), ids),
        sessions: session_views(&core.workspace.read_sessions().expect("read sessions"), ids),
    }
}

/// Wait until the published watch lists (the sidebar feeds) show `expected`.
async fn published_view(core: &EngineCore, ids: &Ids, expected: &Canonical) {
    let mut chats = core.workspace.watch_chats();
    let mut spaces = core.workspace.watch_spaces();
    let mut sessions = core.workspace.watch_session_rows();
    tokio::time::timeout(Duration::from_secs(20), async {
        chats
            .wait_for(|list| chat_views(list, ids) == expected.chats)
            .await
            .map(|_| ())
            .expect("chat watch open");
        spaces
            .wait_for(|list| space_views(list, ids) == expected.spaces)
            .await
            .map(|_| ())
            .expect("space watch open");
        sessions
            .wait_for(|list| session_views(list, ids) == expected.sessions)
            .await
            .map(|_| ())
            .expect("session watch open");
    })
    .await
    .expect("published sidebar lists match the oracle");
}

/// Plain-JSON projection of persisted or server rows (wire field names).
fn rows_view<'a>(rows: impl IntoIterator<Item = &'a Value>, ids: &Ids) -> Canonical {
    let mut view = Canonical::default();
    for row in rows {
        if row["deleted"].as_bool() == Some(true) {
            continue;
        }
        let fields = &row["fields"];
        let id = row["id"].as_str().expect("row id").to_string();
        match row["kind"].as_str() {
            Some("spaces") if ids.spaces.contains(&id) => view.spaces.push(SpaceView {
                id,
                name: fields["name"].as_str().map(str::to_string),
                removed: !fields["removedAt"].is_null(),
            }),
            Some("chats") if ids.chats.contains(&id) => view.chats.push(ChatView {
                id,
                space_id: fields["spaceId"].as_str().map(str::to_string),
                title: fields["title"].as_str().map(str::to_string),
                archived: fields["archived"].as_bool().unwrap_or(false),
                pinned: !fields["pinnedOrder"].is_null(),
                settled: fields["settlementOverride"]["disposition"]
                    .as_str()
                    .map(|disposition| disposition == "settled"),
            }),
            Some("sessions") if ids.chats.contains(&id) => view.sessions.push(SessionView {
                chat_id: id,
                status: fields["status"].as_str().expect("status").to_string(),
                updated_at: fields["updatedAt"].as_i64().expect("updatedAt"),
                last_completed_turn: fields["lastCompletedTurn"].as_str().map(str::to_string),
            }),
            _ => {}
        }
    }
    view.spaces.sort_by(|a, b| a.id.cmp(&b.id));
    view.chats.sort_by(|a, b| a.id.cmp(&b.id));
    view.sessions.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
    view
}

fn snapshot_view(snapshot: &Value, ids: &Ids) -> Canonical {
    rows_view(snapshot["rows"].as_array().expect("rows array"), ids)
}

fn server_view(server: &MockRegistryServer, ids: &Ids) -> Canonical {
    let mut rows = Vec::new();
    for space in &ids.spaces {
        rows.extend(server.row("spaces", space));
    }
    for chat in &ids.chats {
        rows.extend(server.row("chats", chat));
        rows.extend(server.row("sessions", chat));
    }
    let rows: Vec<Value> = rows
        .iter()
        .map(|row| serde_json::to_value(row).expect("server row JSON"))
        .collect();
    rows_view(&rows, ids)
}

fn session_row(
    chat: &str,
    device: &str,
    status: SessionStatus,
    updated_ms: i64,
    turn: Option<&str>,
) -> Session {
    Session {
        last_completed_turn: turn.map(str::to_string),
        chat_id: chat.to_string(),
        device_id: device.to_string(),
        status,
        started_at: Some(ts(BASE_MS)),
        updated_at: ts(updated_ms),
        recovery: None,
    }
}

// ── growth fixture: a fixed row set and an oracle of its current values ────

#[derive(Default)]
struct Model {
    spaces: BTreeMap<String, SpaceView>,
    paths: BTreeMap<String, String>,
    chats: BTreeMap<String, ChatView>,
    sessions: BTreeMap<String, SessionView>,
}

impl Model {
    fn ids(&self) -> Ids {
        Ids {
            spaces: self.spaces.keys().cloned().collect(),
            chats: self.chats.keys().cloned().collect(),
        }
    }

    fn canonical(&self) -> Canonical {
        Canonical {
            spaces: self.spaces.values().cloned().collect(),
            chats: self.chats.values().cloned().collect(),
            sessions: self.sessions.values().cloned().collect(),
        }
    }
}

async fn execute(client: &RpcClient, command: Value) -> CommandReceipt {
    let receipt: CommandReceipt = client
        .call_as(methods::EXECUTE_TASK_COMMAND, command)
        .await
        .expect("task command RPC");
    assert_eq!(
        receipt.status,
        CommandReceiptStatus::Applied,
        "task command was not applied: {receipt:?}"
    );
    receipt
}

async fn task_revision(client: &RpcClient, task: &str) -> u64 {
    let read: TaskCommandRead = client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            json!({ "taskId": task }),
        )
        .await
        .expect("task command snapshot");
    read.snapshot.task_revision
}

async fn create_task(client: &RpcClient, task: &str, space: &str) {
    execute(
        client,
        json!({
            "commandId": format!("zi02-create-{task}"),
            "taskId": task,
            "intent": { "kind": "createChat", "spaceId": space },
        }),
    )
    .await;
}

/// Two projects and eight tasks created through the real engine paths.
async fn create_fixture(core: &EngineCore, client: &RpcClient, root: &Path) -> Model {
    let mut model = Model::default();
    for index in 0..SPACES {
        let id = format!("zi02-space-{index}");
        let path = root.join("projects").join(format!("p{index}"));
        std::fs::create_dir_all(&path).expect("project dir");
        let path = path.to_str().expect("utf-8 path").to_string();
        let name = "s-000000".to_string();
        core.workspace
            .create_space(&id, &core.device_id, &path, Some(name.clone()), false)
            .expect("create project");
        model.paths.insert(id.clone(), path);
        model.spaces.insert(
            id.clone(),
            SpaceView {
                id,
                name: Some(name),
                removed: false,
            },
        );
    }
    for index in 0..CHATS {
        let id = format!("zi02-chat-{index}");
        let space = format!("zi02-space-{}", index % SPACES);
        create_task(client, &id, &space).await;
        core.workspace.record_session(&session_row(
            &id,
            &core.device_id,
            SessionStatus::Idle,
            BASE_MS,
            None,
        ));
        model.chats.insert(
            id.clone(),
            ChatView {
                id: id.clone(),
                space_id: Some(space),
                title: None,
                archived: false,
                pinned: false,
                settled: None,
            },
        );
        model.sessions.insert(
            id.clone(),
            SessionView {
                chat_id: id,
                status: "idle".into(),
                updated_at: BASE_MS,
                last_completed_turn: None,
            },
        );
    }
    model
}

/// One guarded task command: rename, archive/restore, settle/reactivate or
/// pin/unpin (placement), cycling.
async fn rpc_write(client: &RpcClient, model: &mut Model, index: usize, chat: &str) {
    let revision = task_revision(client, chat).await;
    let view = model.chats.get_mut(chat).expect("fixture chat");
    let intent = match (index / RPC_EVERY) % 4 {
        0 => {
            let title = format!("r-{index:06}");
            view.title = Some(title.clone());
            json!({ "kind": "rename", "title": title })
        }
        1 => {
            view.archived = !view.archived;
            if view.archived {
                view.pinned = false;
            }
            json!({ "kind": "setArchived", "archived": view.archived })
        }
        2 => {
            let settled = view.settled != Some(true);
            view.settled = Some(settled);
            json!({ "kind": "setSettled", "settled": settled })
        }
        _ => {
            view.pinned = !view.pinned;
            json!({ "kind": "setPinned", "pinned": view.pinned })
        }
    };
    execute(
        client,
        json!({
            "commandId": format!("zi02-rpc-{index}"),
            "taskId": chat,
            "expectedTaskRevision": revision,
            "intent": intent,
        }),
    )
    .await;
}

/// Repeated supported updates to the fixed row set, round-robin over the
/// tasks so consecutive writes never touch the same row. Returns how many
/// went through the task-command RPC.
async fn apply_writes(
    core: &EngineCore,
    client: &RpcClient,
    model: &mut Model,
    from: usize,
    to: usize,
    rpc: bool,
) -> usize {
    let mut rpc_writes = 0;
    for index in from..to {
        let chat = format!("zi02-chat-{}", index % CHATS);
        if rpc && index % RPC_EVERY == RPC_EVERY - 1 {
            rpc_write(client, model, index, &chat).await;
            rpc_writes += 1;
            continue;
        }
        let space = model.chats[&chat].space_id.clone().expect("fixture space");
        match (index / CHATS) % 5 {
            0 => {
                let title = format!("t-{index:06}");
                assert!(core.workspace.rename_chat(&chat, &title).unwrap());
                model.chats.get_mut(&chat).unwrap().title = Some(title);
            }
            1 => {
                let archived = (index / (CHATS * 5)) % 2 == 0;
                assert!(core.workspace.set_chat_archived(&chat, archived).unwrap());
                let view = model.chats.get_mut(&chat).unwrap();
                view.archived = archived;
                if archived {
                    view.pinned = false;
                }
            }
            2 => {
                let status = [
                    SessionStatus::Working,
                    SessionStatus::AwaitingInput,
                    SessionStatus::Working,
                    SessionStatus::Idle,
                ][(index / (CHATS * 5)) % 4];
                let view = model.sessions.get_mut(&chat).unwrap();
                if status == SessionStatus::Idle {
                    view.last_completed_turn = Some(format!("turn-{index:06}"));
                }
                view.status = status_name(status).to_string();
                view.updated_at = BASE_MS + index as i64;
                core.workspace.record_session(&session_row(
                    &chat,
                    &core.device_id,
                    status,
                    view.updated_at,
                    view.last_completed_turn.as_deref(),
                ));
            }
            3 => {
                let name = format!("s-{index:06}");
                assert!(
                    core.workspace
                        .rename_space(&space, Some(name.as_str()))
                        .unwrap()
                );
                model.spaces.get_mut(&space).unwrap().name = Some(name);
            }
            _ => {
                let path = model.paths[&space].clone();
                let view = model.spaces.get_mut(&space).unwrap();
                if view.removed {
                    // Restoring a hidden project reuses its row.
                    core.workspace
                        .create_space(&space, &core.device_id, &path, None, false)
                        .unwrap();
                } else {
                    assert!(core.workspace.remove_space(&space).unwrap());
                }
                view.removed = !view.removed;
            }
        }
    }
    rpc_writes
}

/// F1/F5/F6 (local) and the before/after growth measurement. A permanently
/// local profile takes 25,000 then 100,000 repeated updates (rename,
/// terminal/live status, archive/restore, project hide/restore and, through
/// guarded task commands, settle and pin placement) with an empty persisted
/// queue and snapshot bytes that follow the row values, not the write count;
/// a restart reproduces the oracle in reads and published lists. A
/// sync-capable development profile on the same fixture and write pattern —
/// the pre-ZI-02 behaviour of every profile — keeps one queued op per write.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_profile_bounds_history_across_100k_writes_and_restart() {
    let root = tempfile::tempdir().expect("tempdir");

    // ── after: permanently local profile ──
    let local_dir = root.path().join("local");
    pin_device(&local_dir, "zi02-growth-local");
    let profile = EngineProfile::local(&local_dir).expect("local profile");
    let db = database(&profile);
    let core = assemble(profile.clone());
    let client = luna_rpc::memory_client(core.rpc_service());
    let mut model = create_fixture(&core, &client, &local_dir).await;
    let ids = model.ids();
    let mut local_points = Vec::new();
    let mut written = 0;
    let mut rpc_writes = 0;
    let mut stats_at = Vec::new();
    for target in LOCAL_CHECKPOINTS {
        let started = Instant::now();
        rpc_writes += apply_writes(&core, &client, &mut model, written, target, true).await;
        let write_ms = millis(started);
        written = target;
        let started = Instant::now();
        core.workspace.flush();
        let flush_ms = millis(started);
        let (bytes, raw) = raw_snapshot(&db);
        let stats = queue_stats(&bytes, &raw);
        let expected = model.canonical();
        assert_eq!(raw["mode"], json!("localOnly"), "local marker persisted");
        assert_eq!(
            stats.pending_batches, 0,
            "a local profile queues nothing at {target} writes"
        );
        assert_eq!(raw["serverSeq"], json!(0), "no server ever applied a frame");
        let persisted = snapshot_view(&raw, &ids);
        let public = public_view(&core, &ids);
        assert_eq!(persisted, expected, "persisted rows at {target} writes");
        assert_eq!(public, expected, "public reads at {target} writes");
        stats_at.push(stats);
        local_points.push(json!({
            "writes": target,
            "rpcWrites": rpc_writes,
            "queue": stats,
            "writeMs": write_ms,
            "flushMs": flush_ms,
            "expectedHash": expected.hash(),
            "persistedHash": persisted.hash(),
            "publicHash": public.hash(),
        }));
    }
    assert!(core.workspace.sync_status().is_none(), "no registry room");
    let (at_25k, at_100k) = (stats_at[0], stats_at[1]);
    assert_eq!(at_25k.rows, at_100k.rows, "the row set is fixed");
    // Values are fixed-width; only pin order keys may lengthen slightly.
    assert!(
        at_100k.snapshot_bytes <= at_25k.snapshot_bytes + 4_096,
        "75,000 more writes grew the snapshot from {} to {} bytes",
        at_25k.snapshot_bytes,
        at_100k.snapshot_bytes
    );
    let expected = model.canonical();
    published_view(&core, &ids, &expected).await;
    drop(client);
    let started = Instant::now();
    shutdown(core).await;
    let shutdown_ms = millis(started);

    let started = Instant::now();
    let core = assemble(profile.clone());
    let reopen_ms = millis(started);
    assert_eq!(public_view(&core, &ids), expected, "reads after restart");
    published_view(&core, &ids, &expected).await;
    core.workspace.flush();
    let (bytes, raw) = raw_snapshot(&db);
    let restarted = queue_stats(&bytes, &raw);
    assert_eq!(restarted.pending_batches, 0);
    assert_eq!(raw["mode"], json!("localOnly"), "restart keeps the mode");
    assert_eq!(
        snapshot_view(&raw, &ids),
        expected,
        "persisted rows after restart"
    );
    shutdown(core).await;

    // ── before: the same writes on a sync-capable, edge-less profile ──
    let replica_dir = root.path().join("replica");
    pin_device(&replica_dir, "zi02-growth-replica");
    let profile = EngineProfile::development(&replica_dir, ORG, USER);
    let db = database(&profile);
    let core = assemble(profile.clone());
    let client = luna_rpc::memory_client(core.rpc_service());
    let mut model = create_fixture(&core, &client, &replica_dir).await;
    let ids = model.ids();
    core.workspace.flush();
    let (bytes, raw) = raw_snapshot(&db);
    let fixture_ops = queue_stats(&bytes, &raw).pending_ops;
    let mut replica_points = Vec::new();
    let mut replica_stats = Vec::new();
    let mut written = 0;
    for target in REPLICA_CHECKPOINTS {
        let started = Instant::now();
        apply_writes(&core, &client, &mut model, written, target, false).await;
        let write_ms = millis(started);
        written = target;
        let started = Instant::now();
        core.workspace.flush();
        let flush_ms = millis(started);
        let (bytes, raw) = raw_snapshot(&db);
        let stats = queue_stats(&bytes, &raw);
        assert!(
            raw.get("mode").is_none(),
            "a replica never becomes local-only"
        );
        assert!(
            stats.pending_ops >= fixture_ops + target,
            "an offline replica keeps every write's intent ({} ops for {target} writes)",
            stats.pending_ops
        );
        let public = public_view(&core, &ids);
        assert_eq!(
            public,
            model.canonical(),
            "replica overlay at {target} writes"
        );
        replica_stats.push(stats);
        replica_points.push(json!({
            "writes": target,
            "queue": stats,
            "writeMs": write_ms,
            "flushMs": flush_ms,
            "publicHash": public.hash(),
        }));
    }
    let (small, large) = (replica_stats[0], replica_stats[1]);
    let per_write = large.snapshot_bytes.saturating_sub(small.snapshot_bytes) as f64
        / (REPLICA_CHECKPOINTS[1] - REPLICA_CHECKPOINTS[0]) as f64;
    assert!(
        per_write >= 100.0,
        "the baseline grows with history ({per_write:.1} bytes per write)"
    );
    drop(client);
    shutdown(core).await;
    let started = Instant::now();
    let core = assemble(profile);
    let replica_reopen_ms = millis(started);
    assert_eq!(public_view(&core, &ids), model.canonical());
    shutdown(core).await;

    retain_evidence(
        "registry-growth-e2e.json",
        &json!({
            "test": "local_profile_bounds_history_across_100k_writes_and_restart",
            "version": env!("CARGO_PKG_VERSION"),
            "sourceSha": std::env::var("LUNA_SOURCE_SHA").ok(),
            "fixture": { "spaces": SPACES, "chats": CHATS, "sessions": CHATS, "rpcEvery": RPC_EVERY },
            "local": {
                "checkpoints": local_points,
                "shutdownMs": shutdown_ms,
                "reopenMs": reopen_ms,
                "afterRestart": restarted,
                "restartHash": expected.hash(),
            },
            "replicaBaseline": {
                "fixtureOps": fixture_ops,
                "checkpoints": replica_points,
                "bytesPerWrite": per_write,
                "projectedBytesAt100k": (per_write * 100_000.0) as u64,
                "reopenMs": replica_reopen_ms,
            },
        }),
    );
}

// ── migration fixture: an older queued local snapshot ──────────────────────

const LEGACY_DEVICE: &str = "zi02-legacy-device";

/// The persisted HLC format: `{ms:013}-{counter:06}-{device}`.
fn clock(ms: i64, device: &str) -> String {
    format!("{ms:013}-000000-{device}")
}

fn legacy_row(kind: &str, id: &str, fields: Value, ms: i64) -> Value {
    let clocks: serde_json::Map<String, Value> = fields
        .as_object()
        .expect("row fields")
        .keys()
        .map(|key| (key.clone(), json!(clock(ms, LEGACY_DEVICE))))
        .collect();
    json!({ "kind": kind, "id": id, "seq": 0, "deleted": false, "fields": fields, "clocks": clocks })
}

fn set_op(kind: &str, id: &str, op: &str, fields: Value, hlc: String) -> Value {
    json!({ "kind": kind, "id": id, "op": op, "set": fields, "hlc": hlc })
}

fn delete_op(kind: &str, id: &str, hlc: String) -> Value {
    json!({ "kind": kind, "id": id, "op": "delete", "hlc": hlc })
}

fn legacy_snapshot(rows: Vec<Value>, batches: Vec<Vec<Value>>, last_ms: i64) -> Vec<u8> {
    let pending: Vec<Value> = batches
        .into_iter()
        .enumerate()
        .map(|(index, ops)| json!({ "batch": format!("b-legacy-{index}"), "ops": ops }))
        .collect();
    serde_json::to_vec(&json!({
        "v": 1,
        "resyncEpoch": 1,
        "deviceId": LEGACY_DEVICE,
        "serverSeq": 0,
        "gcFloor": 0,
        "clock": { "last_ms": last_ms, "counter": 0 },
        "rows": rows,
        "pending": pending,
    }))
    .expect("legacy snapshot JSON")
}

fn store_legacy_snapshot(profile: &EngineProfile, bytes: &[u8]) {
    let store = luna_sync::DocsStore::open(profile.store_root()).expect("open legacy store");
    store
        .save_snapshot("registry1", bytes)
        .expect("store legacy snapshot");
}

/// Acked rows plus a queue an older local build never retired: migration
/// seeds, repeated renames, a terminal status, an atomic cascade, a stale
/// upsert under a tombstone, an update on a tombstone, a stale live status,
/// archive/restore and a guard tombstone for a never-created row.
fn queued_local_fixture() -> Vec<u8> {
    let at = |ms| clock(ms, LEGACY_DEVICE);
    let migration = clock(500, "migration");
    let rows = vec![
        legacy_row(
            "spaces",
            "zi02-sp-keep",
            json!({ "id": "zi02-sp-keep", "deviceId": LEGACY_DEVICE, "path": "/zi02/keep", "name": "Keep", "createdAt": 1_000 }),
            1_000,
        ),
        legacy_row(
            "chats",
            "zi02-c-live",
            json!({ "id": "zi02-c-live", "deviceId": LEGACY_DEVICE, "title": "original", "archived": false, "cwd": "/zi02/keep", "createdAt": 1_000, "spaceId": "zi02-sp-keep" }),
            1_000,
        ),
    ];
    let session = |status: &str, turn: Option<&str>, updated: i64| {
        let mut fields = json!({ "chatId": "zi02-c-live", "deviceId": LEGACY_DEVICE, "status": status, "updatedAt": updated });
        if let Some(turn) = turn {
            fields["lastCompletedTurn"] = json!(turn);
        }
        fields
    };
    let batches = vec![
        vec![
            set_op(
                "spaces",
                "zi02-sp-gone",
                "upsert",
                json!({ "id": "zi02-sp-gone", "deviceId": LEGACY_DEVICE, "path": "/zi02/gone", "name": "Gone", "createdAt": 500 }),
                migration.clone(),
            ),
            set_op(
                "chats",
                "zi02-c-gone-1",
                "upsert",
                json!({ "id": "zi02-c-gone-1", "deviceId": LEGACY_DEVICE, "title": "gone one", "spaceId": "zi02-sp-gone", "createdAt": 500 }),
                migration.clone(),
            ),
            set_op(
                "chats",
                "zi02-c-gone-2",
                "upsert",
                json!({ "id": "zi02-c-gone-2", "deviceId": LEGACY_DEVICE, "title": "gone two", "spaceId": "zi02-sp-gone", "createdAt": 500 }),
                migration.clone(),
            ),
            set_op(
                "sessions",
                "zi02-c-gone-1",
                "upsert",
                json!({ "chatId": "zi02-c-gone-1", "deviceId": LEGACY_DEVICE, "status": "working", "updatedAt": 500 }),
                migration,
            ),
        ],
        vec![set_op(
            "chats",
            "zi02-c-live",
            "update",
            json!({ "title": "v1" }),
            at(2_000),
        )],
        vec![set_op(
            "chats",
            "zi02-c-live",
            "update",
            json!({ "title": "v2" }),
            at(3_000),
        )],
        vec![set_op(
            "chats",
            "zi02-c-live",
            "update",
            json!({ "title": "v3" }),
            at(4_000),
        )],
        vec![set_op(
            "sessions",
            "zi02-c-live",
            "upsert",
            session("idle", Some("turn-final"), 5_000),
            at(5_000),
        )],
        vec![
            delete_op("chats", "zi02-c-gone-1", at(6_000)),
            delete_op("sessions", "zi02-c-gone-1", at(6_000)),
            delete_op("chats", "zi02-c-gone-2", at(6_000)),
            delete_op("sessions", "zi02-c-gone-2", at(6_000)),
            delete_op("spaces", "zi02-sp-gone", at(6_000)),
        ],
        vec![set_op(
            "chats",
            "zi02-c-gone-1",
            "upsert",
            json!({ "id": "zi02-c-gone-1", "deviceId": LEGACY_DEVICE, "title": "stale" }),
            at(5_500),
        )],
        vec![set_op(
            "chats",
            "zi02-c-gone-2",
            "update",
            json!({ "title": "ghost" }),
            at(7_000),
        )],
        vec![set_op(
            "sessions",
            "zi02-c-live",
            "upsert",
            session("working", None, 4_500),
            at(4_500),
        )],
        vec![set_op(
            "chats",
            "zi02-c-live",
            "update",
            json!({ "archived": true, "pinnedOrder": null }),
            at(8_000),
        )],
        vec![set_op(
            "chats",
            "zi02-c-live",
            "update",
            json!({ "archived": false }),
            at(9_000),
        )],
        vec![delete_op("chats", "zi02-c-temp", at(9_500))],
    ];
    legacy_snapshot(rows, batches, 9_500)
}

/// The fixture's effective state, written out by hand.
fn queued_local_expected() -> Canonical {
    Canonical {
        spaces: vec![SpaceView {
            id: "zi02-sp-keep".into(),
            name: Some("Keep".into()),
            removed: false,
        }],
        chats: vec![ChatView {
            id: "zi02-c-live".into(),
            space_id: Some("zi02-sp-keep".into()),
            title: Some("v3".into()),
            archived: false,
            pinned: false,
            settled: None,
        }],
        sessions: vec![SessionView {
            chat_id: "zi02-c-live".into(),
            status: "idle".into(),
            updated_at: 5_000,
            last_completed_turn: Some("turn-final".into()),
        }],
    }
}

/// F4/F5/F6 (migration). An older local snapshot with a 12-batch queue folds
/// in its original semantic order — HLCs, tombstones, the terminal status and
/// the cascade preserved — but only through the guarded boot save: with that
/// save failing the open fails and the stored queue is byte-identical. A
/// later guarded task-command rollback restores the local mode, so the next
/// write still folds, and a second restart reproduces the oracle. A large
/// queue folds once and leaves row-sized bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn older_local_queue_migrates_once_through_guarded_persistence() {
    let root = tempfile::tempdir().expect("tempdir");
    let data = root.path().join("data");
    pin_device(&data, LEGACY_DEVICE);
    let profile = EngineProfile::local(&data).expect("local profile");
    let db = database(&profile);
    let fixture = queued_local_fixture();
    store_legacy_snapshot(&profile, &fixture);
    let fixture_stats = queue_stats(&fixture, &serde_json::from_slice(&fixture).unwrap());
    let ids = Ids::new(
        &["zi02-sp-keep", "zi02-sp-gone"],
        &[
            "zi02-c-live",
            "zi02-c-gone-1",
            "zi02-c-gone-2",
            "zi02-c-temp",
        ],
    );
    let expected = queued_local_expected();

    // The guarded boot save fails: no engine, and the queue is not retired.
    reject_registry_writes(&db);
    assert!(
        try_assemble(profile.clone()).is_err(),
        "a migration that cannot commit must not open"
    );
    let (after_failure, _) = raw_snapshot(&db);
    assert_eq!(
        sha256_hex(&after_failure),
        sha256_hex(&fixture),
        "the stored queue is untouched by a failed migration"
    );
    accept_registry_writes(&db);

    let started = Instant::now();
    let core = assemble(profile.clone());
    let migrate_ms = millis(started);
    let public = public_view(&core, &ids);
    assert_eq!(public, expected, "migrated reads equal the fixture's state");
    published_view(&core, &ids, &expected).await;
    let (bytes, raw) = raw_snapshot(&db);
    let migrated = queue_stats(&bytes, &raw);
    assert_eq!(raw["mode"], json!("localOnly"));
    assert_eq!(
        migrated.pending_batches, 0,
        "the boot save retired the queue"
    );
    let persisted = snapshot_view(&raw, &ids);
    assert_eq!(persisted, expected);
    // Original clocks and tombstones survive the fold.
    let c = |ms| json!(clock(ms, LEGACY_DEVICE));
    let live = raw_row(&raw, "chats", "zi02-c-live");
    assert_eq!(live["clocks"]["title"], c(4_000));
    assert_eq!(live["clocks"]["archived"], c(9_000));
    assert_eq!(live["clocks"]["pinnedOrder"], c(8_000));
    assert_eq!(live["clocks"]["cwd"], c(1_000));
    let status = raw_row(&raw, "sessions", "zi02-c-live");
    assert_eq!(status["clocks"]["status"], c(5_000));
    assert_eq!(status["clocks"]["lastCompletedTurn"], c(5_000));
    for (kind, id, at) in [
        ("chats", "zi02-c-gone-1", 6_000),
        ("chats", "zi02-c-gone-2", 6_000),
        ("sessions", "zi02-c-gone-1", 6_000),
        ("sessions", "zi02-c-gone-2", 6_000),
        ("spaces", "zi02-sp-gone", 6_000),
        ("chats", "zi02-c-temp", 9_500),
    ] {
        let row = raw_row(&raw, kind, id);
        assert_eq!(row["deleted"], json!(true), "{kind}/{id} stays deleted");
        assert_eq!(
            row["delHlc"],
            c(at),
            "{kind}/{id} keeps its tombstone clock"
        );
    }

    // A guarded task-command commit fails and rolls back in memory; the
    // restored doc must still be local-only.
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task(&client, "zi02-c-rpc", "zi02-sp-keep").await;
    let revision = task_revision(&client, "zi02-c-rpc").await;
    reject_registry_writes(&db);
    let rolled_back = client
        .call(
            methods::EXECUTE_TASK_COMMAND,
            json!({
                "commandId": "zi02-rollback-rename",
                "taskId": "zi02-c-rpc",
                "expectedTaskRevision": revision,
                "intent": { "kind": "rename", "title": "must roll back" },
            }),
        )
        .await;
    accept_registry_writes(&db);
    let error = rolled_back.expect_err("the projection commit must fail");
    assert!(error.to_string().contains("rolled back"), "{error}");
    assert!(
        core.workspace
            .rename_chat("zi02-c-live", "after-rollback")
            .unwrap()
    );
    core.workspace.flush();
    let (bytes, raw) = raw_snapshot(&db);
    let after_rollback = queue_stats(&bytes, &raw);
    assert_eq!(raw["mode"], json!("localOnly"), "rollback keeps the mode");
    assert_eq!(
        after_rollback.pending_batches, 0,
        "writes after a rollback still fold"
    );
    let mut expected = expected;
    expected.chats[0].title = Some("after-rollback".into());
    expected.chats.push(ChatView {
        id: "zi02-c-rpc".into(),
        space_id: Some("zi02-sp-keep".into()),
        title: None,
        archived: false,
        pinned: false,
        settled: None,
    });
    let mut ids = ids;
    ids.chats.insert("zi02-c-rpc".into());
    assert_eq!(snapshot_view(&raw, &ids), expected);
    assert_eq!(public_view(&core, &ids), expected);
    drop(client);
    shutdown(core).await;

    // Second restart.
    let core = assemble(profile);
    assert_eq!(
        public_view(&core, &ids),
        expected,
        "reads after the second restart"
    );
    core.workspace.flush();
    let (bytes, raw) = raw_snapshot(&db);
    let second_restart = queue_stats(&bytes, &raw);
    assert_eq!(second_restart.pending_batches, 0);
    assert_eq!(snapshot_view(&raw, &ids), expected);
    shutdown(core).await;

    // A large never-acked queue (repeated renames of a fixed row set) folds
    // once, at startup, to row-sized bytes.
    const QUEUED: usize = 50_000;
    let big = root.path().join("big");
    pin_device(&big, LEGACY_DEVICE);
    let big_profile = EngineProfile::local(&big).expect("local profile");
    let big_db = database(&big_profile);
    let mut big_ids = Ids::new(&["zi02-sp-keep"], &[]);
    let mut rows = vec![legacy_row(
        "spaces",
        "zi02-sp-keep",
        json!({ "id": "zi02-sp-keep", "deviceId": LEGACY_DEVICE, "path": "/zi02/keep", "name": "Keep", "createdAt": 1_000 }),
        1_000,
    )];
    let mut big_expected = Canonical {
        spaces: vec![SpaceView {
            id: "zi02-sp-keep".into(),
            name: Some("Keep".into()),
            removed: false,
        }],
        ..Canonical::default()
    };
    for index in 0..CHATS {
        let id = format!("zi02-big-{index}");
        rows.push(legacy_row(
            "chats",
            &id,
            json!({ "id": id, "deviceId": LEGACY_DEVICE, "title": "t-000000", "createdAt": 1_000, "spaceId": "zi02-sp-keep" }),
            1_000,
        ));
        big_ids.chats.insert(id.clone());
        let last = (0..QUEUED)
            .rev()
            .find(|n| n % CHATS == index)
            .expect("last write");
        big_expected.chats.push(ChatView {
            id,
            space_id: Some("zi02-sp-keep".into()),
            title: Some(format!("t-{last:06}")),
            archived: false,
            pinned: false,
            settled: None,
        });
    }
    let batches: Vec<Vec<Value>> = (0..QUEUED)
        .map(|n| {
            vec![set_op(
                "chats",
                &format!("zi02-big-{}", n % CHATS),
                "update",
                json!({ "title": format!("t-{n:06}") }),
                clock(2_000 + n as i64, LEGACY_DEVICE),
            )]
        })
        .collect();
    let big_fixture = legacy_snapshot(rows, batches, 2_000 + QUEUED as i64);
    store_legacy_snapshot(&big_profile, &big_fixture);
    let big_before = queue_stats(&big_fixture, &serde_json::from_slice(&big_fixture).unwrap());
    let started = Instant::now();
    let core = assemble(big_profile);
    let big_migrate_ms = millis(started);
    assert_eq!(public_view(&core, &big_ids), big_expected);
    let (bytes, raw) = raw_snapshot(&big_db);
    let big_after = queue_stats(&bytes, &raw);
    assert_eq!(big_after.pending_batches, 0);
    assert_eq!(snapshot_view(&raw, &big_ids), big_expected);
    assert!(
        big_after.snapshot_bytes < 64 * 1024,
        "{QUEUED} folded ops left {} bytes",
        big_after.snapshot_bytes
    );
    shutdown(core).await;

    retain_evidence(
        "registry-migration-e2e.json",
        &json!({
            "test": "older_local_queue_migrates_once_through_guarded_persistence",
            "version": env!("CARGO_PKG_VERSION"),
            "sourceSha": std::env::var("LUNA_SOURCE_SHA").ok(),
            "fixture": { "sha256": sha256_hex(&fixture), "queue": fixture_stats },
            "failedBootSave": { "openFailed": true, "storedSha256": sha256_hex(&after_failure) },
            "migrated": { "queue": migrated, "ms": migrate_ms, "expectedHash": queued_local_expected().hash(), "publicHash": public.hash(), "persistedHash": persisted.hash() },
            "afterGuardedRollback": { "queue": after_rollback, "hash": expected.hash() },
            "secondRestart": { "queue": second_restart },
            "largeQueue": { "queuedOps": QUEUED, "before": big_before, "after": big_after, "ms": big_migrate_ms, "hash": big_expected.hash() },
        }),
    );
}

// ── reconnect: real transport, a frame-withholding link, an observer ───────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkMode {
    Forward,
    /// Forward everything except `ack` frames, so pushed batches stay in flight.
    HoldAcks,
    /// Refuse new connections (the device is offline).
    Refuse,
}

/// A WebSocket relay between one engine and the mock registry server. It
/// only withholds `ack` frames, refuses connections or severs live ones; it
/// never synthesizes or rewrites a frame.
struct RegistryLink {
    addr: SocketAddr,
    mode: Arc<Mutex<LinkMode>>,
    sever: tokio::sync::watch::Sender<u64>,
    accept: tokio::task::JoinHandle<()>,
}

impl RegistryLink {
    async fn start(upstream: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind link");
        let addr = listener.local_addr().expect("link address");
        let mode = Arc::new(Mutex::new(LinkMode::Forward));
        let (sever, _) = tokio::sync::watch::channel(0u64);
        let accept_mode = mode.clone();
        let accept_sever = sever.clone();
        let accept = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                if *accept_mode.lock().unwrap() == LinkMode::Refuse {
                    drop(stream);
                    continue;
                }
                tokio::spawn(relay(
                    stream,
                    upstream.clone(),
                    accept_mode.clone(),
                    accept_sever.subscribe(),
                ));
            }
        });
        Self {
            addr,
            mode,
            sever,
            accept,
        }
    }

    fn url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    fn set(&self, mode: LinkMode) {
        *self.mode.lock().unwrap() = mode;
    }

    fn sever(&self) {
        self.sever.send_modify(|generation| *generation += 1);
    }
}

impl Drop for RegistryLink {
    fn drop(&mut self) {
        self.accept.abort();
        self.sever();
    }
}

fn frame_kind(text: &str) -> Option<String> {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|frame| frame["t"].as_str().map(str::to_string))
}

async fn relay(
    stream: TcpStream,
    upstream: String,
    mode: Arc<Mutex<LinkMode>>,
    mut sever: tokio::sync::watch::Receiver<u64>,
) {
    let Ok(client) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let Ok((server, _)) = tokio_tungstenite::connect_async(upstream.as_str()).await else {
        return;
    };
    let (mut to_client, mut from_client) = client.split();
    let (mut to_server, mut from_server) = server.split();
    loop {
        tokio::select! {
            _ = sever.changed() => return,
            frame = from_client.next() => match frame {
                Some(Ok(message)) => {
                    if to_server.send(message).await.is_err() {
                        return;
                    }
                }
                _ => return,
            },
            frame = from_server.next() => match frame {
                Some(Ok(WsMessage::Text(text))) => {
                    let held = *mode.lock().unwrap() == LinkMode::HoldAcks
                        && frame_kind(&text).as_deref() == Some("ack");
                    if !held && to_client.send(WsMessage::Text(text)).await.is_err() {
                        return;
                    }
                }
                Some(Ok(message)) => {
                    if to_client.send(message).await.is_err() {
                        return;
                    }
                }
                _ => return,
            },
        }
    }
}

/// An independent room member recording every frame the server sends it, in
/// arrival order: the row broadcasts peers derive notifications from.
struct Observer {
    frames: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Observer {
    async fn join(url: &str) -> Self {
        let (mut socket, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("observer joins the room");
        socket
            .send(WsMessage::Text(
                json!({ "t": "hello", "cursor": null, "device": "zi02-observer" }).to_string(),
            ))
            .await
            .expect("observer hello");
        let frames = Arc::new(Mutex::new(Vec::new()));
        let sink = frames.clone();
        let task = tokio::spawn(async move {
            while let Some(Ok(message)) = socket.next().await {
                if let WsMessage::Text(text) = message
                    && let Ok(frame) = serde_json::from_str::<Value>(&text)
                {
                    sink.lock().unwrap().push(frame);
                }
            }
        });
        let observer = Self { frames, task };
        wait_until("the observer's state frame", || {
            observer
                .frames
                .lock()
                .unwrap()
                .iter()
                .any(|f| f["t"] == "state")
        })
        .await;
        observer
    }

    fn mark(&self) -> usize {
        self.frames.lock().unwrap().len()
    }

    fn rows_since(&self, mark: usize) -> Vec<Value> {
        self.frames.lock().unwrap()[mark..]
            .iter()
            .filter(|frame| frame["t"] == "rows")
            .cloned()
            .collect()
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The peer-visible sequence for one task: title transitions of its chat row
/// and every session-row frame, in broadcast order.
fn task_feed(frames: &[Value], chat: &str) -> Vec<(String, String)> {
    let mut feed = Vec::new();
    let mut title: Option<String> = None;
    for frame in frames {
        for row in frame["rows"].as_array().into_iter().flatten() {
            if row["id"] != chat {
                continue;
            }
            match row["kind"].as_str() {
                Some("chats") => {
                    let next = row["fields"]["title"].as_str().map(str::to_string);
                    if next != title {
                        title = next.clone();
                        feed.push(("title".into(), next.unwrap_or_default()));
                    }
                }
                Some("sessions") => feed.push((
                    "status".into(),
                    row["fields"]["status"].as_str().unwrap_or_default().into(),
                )),
                _ => {}
            }
        }
    }
    feed
}

/// Pending batches that carry this test's intent (device rows aside).
fn intent_batches(snapshot: &Value) -> Vec<Value> {
    snapshot["pending"]
        .as_array()
        .expect("pending array")
        .iter()
        .filter(|batch| {
            batch["ops"]
                .as_array()
                .is_some_and(|ops| ops.iter().any(|op| op["kind"] != "devices"))
        })
        .cloned()
        .collect()
}

fn batches_for<'a>(batches: &'a [Value], kind: &str, id: &str) -> Vec<&'a Value> {
    batches
        .iter()
        .filter(|batch| {
            batch["ops"]
                .as_array()
                .is_some_and(|ops| ops.len() == 1 && ops[0]["kind"] == kind && ops[0]["id"] == id)
        })
        .collect()
}

fn synced_profile(dir: &Path, device: &str) -> EngineProfile {
    pin_device(dir, device);
    EngineProfile::synced(dir, ORG, USER)
}

/// F2/F3/F5 (sync-capable). Two isolated synced engines share the real
/// registry transport; B's link can withhold acks, sever and refuse. B keeps
/// an in-flight batch's id and content, retires only unsent renames that a
/// later rename supersedes, and queues every other offline intent — renames
/// of rows A deleted meanwhile, a delete of a row A renames later, its own
/// atomic cascade and a working → needs-input → working → completed run —
/// across a restart, in order, without ever becoming local-only. After
/// reconnect the server, A and B hold the oracle's rows; no tombstone is
/// resurrected; the observer sees the cascade as one frame and the task's
/// title and status transitions in their original order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_synced_replica_keeps_intent_through_restart_and_reconnect() {
    let server = MockRegistryServer::start().await;
    let link = RegistryLink::start(server.url()).await;
    let observer = Observer::join(&server.url()).await;
    let root = tempfile::tempdir().expect("tempdir");
    let dir_a = root.path().join("a");
    let dir_b = root.path().join("b");
    let a = assemble(synced_profile(&dir_a, "zi02-a"));
    let b_profile = synced_profile(&dir_b, "zi02-b");
    let db_b = database(&b_profile);
    let b = assemble(b_profile.clone());
    a.workspace.connect_registry_url(&server.url());
    b.workspace.connect_registry_url(&link.url());

    let spaces = ["zi02-sp-keep", "zi02-sp-a", "zi02-sp-b"];
    let chats = [
        ("zi02-c1", "zi02-sp-keep"),
        ("zi02-c2", "zi02-sp-keep"),
        ("zi02-c4", "zi02-sp-keep"),
        ("zi02-c5", "zi02-sp-a"),
        ("zi02-c6", "zi02-sp-a"),
        ("zi02-c7", "zi02-sp-b"),
        ("zi02-c8", "zi02-sp-b"),
    ];
    let ids = Ids::new(&spaces, &chats.map(|(chat, _)| chat));
    for space in spaces {
        a.workspace
            .create_space(space, &a.device_id, &format!("/zi02/{space}"), None, false)
            .expect("create project");
    }
    for (chat, space) in chats {
        a.workspace
            .create_chat(chat, Some(space), None, None, None)
            .expect("create task");
    }
    a.workspace.record_session(&session_row(
        "zi02-c1",
        &a.device_id,
        SessionStatus::Idle,
        BASE_MS,
        None,
    ));
    wait_until("B receives A's rows", || {
        let view = public_view(&b, &ids);
        view.chats.len() == chats.len() && view.sessions.len() == 1
    })
    .await;

    // In flight: the server applies B's renames but B never hears the acks.
    link.set(LinkMode::HoldAcks);
    let server_title = |chat: &str, title: &str| {
        server
            .row("chats", chat)
            .is_some_and(|row| row.fields.get("title") == Some(&json!(title)))
    };
    assert!(b.workspace.rename_chat("zi02-c1", "in-flight-1").unwrap());
    wait_until("the server applies in-flight-1", || {
        server_title("zi02-c1", "in-flight-1")
    })
    .await;
    b.workspace.flush();
    let first = intent_batches(&raw_snapshot(&db_b).1);
    let first = batches_for(&first, "chats", "zi02-c1")
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(first.len(), 1);
    assert!(b.workspace.rename_chat("zi02-c1", "in-flight-2").unwrap());
    wait_until("the server applies in-flight-2", || {
        server_title("zi02-c1", "in-flight-2")
    })
    .await;
    b.workspace.flush();
    let in_flight = intent_batches(&raw_snapshot(&db_b).1);
    let in_flight = batches_for(&in_flight, "chats", "zi02-c1")
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(in_flight.len(), 2, "an in-flight batch is never coalesced");
    assert_eq!(in_flight[0], first[0], "it keeps its id and content");

    // Offline: once disconnected nothing is in flight; a newer rename
    // retires both unsent ones and is queued under a fresh id.
    link.set(LinkMode::Refuse);
    link.sever();
    wait_until("B notices the disconnect", || !b.workspace.connected()).await;
    assert!(b.workspace.rename_chat("zi02-c1", "offline-3").unwrap());
    b.workspace.flush();
    let offline = intent_batches(&raw_snapshot(&db_b).1);
    let offline_c1 = batches_for(&offline, "chats", "zi02-c1");
    assert_eq!(offline_c1.len(), 1, "superseded unsent renames retire");
    assert_eq!(offline_c1[0]["ops"][0]["set"]["title"], json!("offline-3"));
    for earlier in &in_flight {
        assert_ne!(offline_c1[0]["batch"], earlier["batch"], "a fresh batch id");
    }

    // A, online, deletes rows B still shows.
    assert!(a.workspace.delete_chat("zi02-c2").unwrap());
    let cascade_a = a
        .workspace
        .delete_space_if_chats("zi02-sp-a", &["zi02-c5".to_string(), "zi02-c6".to_string()])
        .unwrap();
    assert!(cascade_a.existed);
    let server_deleted = |kind: &str, id: &str| server.row(kind, id).is_some_and(|row| row.deleted);
    wait_until("the server applies A's deletions", || {
        server_deleted("chats", "zi02-c2") && server_deleted("spaces", "zi02-sp-a")
    })
    .await;
    // Wall-clock separation so the HLCs below are causally later (an
    // ordering requirement, not a wait for an event).
    tokio::time::sleep(Duration::from_millis(5)).await;

    // B's offline intent.
    assert!(b.workspace.rename_chat("zi02-c2", "ghost-2").unwrap());
    assert!(b.workspace.rename_chat("zi02-c5", "ghost-5").unwrap());
    assert!(b.workspace.rename_chat("zi02-c1", "needs-review").unwrap());
    for (step, status) in [
        SessionStatus::Working,
        SessionStatus::AwaitingInput,
        SessionStatus::Working,
    ]
    .into_iter()
    .enumerate()
    {
        b.workspace.record_session(&session_row(
            "zi02-c1",
            &b.device_id,
            status,
            BASE_MS + 1 + step as i64,
            None,
        ));
    }
    b.workspace.record_session(&session_row(
        "zi02-c1",
        &b.device_id,
        SessionStatus::Idle,
        BASE_MS + 4,
        Some("turn-b"),
    ));
    assert!(b.workspace.rename_chat("zi02-c1", "done-title").unwrap());
    let cascade_b = b
        .workspace
        .delete_space_if_chats("zi02-sp-b", &["zi02-c7".to_string(), "zi02-c8".to_string()])
        .unwrap();
    assert_eq!(cascade_b.chat_ids, ["zi02-c7", "zi02-c8"]);
    assert!(b.workspace.delete_chat("zi02-c4").unwrap());
    tokio::time::sleep(Duration::from_millis(5)).await;
    // A renames the row B deleted, causally after B's delete.
    assert!(a.workspace.rename_chat("zi02-c4", "renamed-by-a").unwrap());
    wait_until("the server applies A's rename", || {
        server_title("zi02-c4", "renamed-by-a")
    })
    .await;

    b.workspace.flush();
    let (bytes, offline_raw) = raw_snapshot(&db_b);
    let offline_stats = queue_stats(&bytes, &offline_raw);
    assert!(
        offline_raw.get("mode").is_none(),
        "an offline synced profile never becomes local-only"
    );
    let queued = intent_batches(&offline_raw);
    // offline-3, two ghost renames, needs-review, four statuses, done-title,
    // B's cascade and B's delete: every intent, one batch each.
    assert_eq!(queued.len(), 11, "{queued:#?}");
    let statuses: Vec<Value> = queued
        .iter()
        .flat_map(|batch| batch["ops"].as_array().cloned().unwrap_or_default())
        .filter(|op| op["kind"] == "sessions" && op["op"] == "upsert")
        .map(|op| op["set"]["status"].clone())
        .collect();
    assert_eq!(
        statuses,
        [
            json!("working"),
            json!("awaitingInput"),
            json!("working"),
            json!("idle")
        ]
    );
    assert!(
        queued
            .iter()
            .any(|batch| batch["ops"].as_array().is_some_and(|ops| ops.len() == 5)),
        "B's cascade stays one atomic batch"
    );

    // Restart while offline: the exact queue survives.
    shutdown(b).await;
    let b = assemble(b_profile);
    b.workspace.flush();
    let reopened = intent_batches(&raw_snapshot(&db_b).1);
    assert_eq!(reopened, queued, "restart keeps the exact offline queue");
    let b_offline = public_view(&b, &ids);
    assert_eq!(
        b_offline
            .chats
            .iter()
            .find(|chat| chat.id == "zi02-c1")
            .and_then(|chat| chat.title.clone()),
        Some("done-title".into()),
        "the offline overlay shows B's intent"
    );

    // Reconnect.
    let expected = Canonical {
        spaces: vec![SpaceView {
            id: "zi02-sp-keep".into(),
            name: None,
            removed: false,
        }],
        chats: vec![
            ChatView {
                id: "zi02-c1".into(),
                space_id: Some("zi02-sp-keep".into()),
                title: Some("done-title".into()),
                archived: false,
                pinned: false,
                settled: None,
            },
            ChatView {
                id: "zi02-c4".into(),
                space_id: Some("zi02-sp-keep".into()),
                title: Some("renamed-by-a".into()),
                archived: false,
                pinned: false,
                settled: None,
            },
        ],
        sessions: vec![SessionView {
            chat_id: "zi02-c1".into(),
            status: "idle".into(),
            updated_at: BASE_MS + 4,
            last_completed_turn: Some("turn-b".into()),
        }],
    };
    let mark = observer.mark();
    link.set(LinkMode::Forward);
    b.workspace.connect_registry_url(&link.url());
    wait_until("B drains its queue and every replica converges", || {
        b.workspace.flush();
        let (_, raw) = raw_snapshot(&db_b);
        raw["pending"].as_array().is_some_and(Vec::is_empty)
            && server_view(&server, &ids) == expected
            && public_view(&a, &ids) == expected
            && public_view(&b, &ids) == expected
    })
    .await;
    for (kind, id) in [
        ("chats", "zi02-c2"),
        ("chats", "zi02-c5"),
        ("spaces", "zi02-sp-a"),
        ("spaces", "zi02-sp-b"),
        ("chats", "zi02-c7"),
    ] {
        assert!(server_deleted(kind, id), "{kind}/{id} is not resurrected");
    }

    let frames = observer.rows_since(mark);
    let feed = task_feed(&frames, "zi02-c1");
    let pair = |kind: &str, value: &str| (kind.to_string(), value.to_string());
    assert_eq!(
        feed,
        vec![
            pair("title", "offline-3"),
            pair("title", "needs-review"),
            pair("status", "working"),
            pair("status", "awaitingInput"),
            pair("status", "working"),
            pair("status", "idle"),
            pair("title", "done-title"),
        ],
        "peers see B's labels and transitions in their original order"
    );
    let cascade_keys: BTreeSet<(String, String)> = [
        ("spaces", "zi02-sp-b"),
        ("chats", "zi02-c7"),
        ("chats", "zi02-c8"),
        ("sessions", "zi02-c7"),
        ("sessions", "zi02-c8"),
    ]
    .iter()
    .map(|(kind, id)| (kind.to_string(), id.to_string()))
    .collect();
    let cascade_frame = frames.iter().find(|frame| {
        let deleted: BTreeSet<(String, String)> = frame["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| row["deleted"] == json!(true))
            .map(|row| {
                (
                    row["kind"].as_str().unwrap_or_default().to_string(),
                    row["id"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        cascade_keys.is_subset(&deleted)
    });
    assert!(cascade_frame.is_some(), "B's cascade applies as one frame");

    let hash = expected.hash();
    retain_evidence(
        "registry-reconnect-e2e.json",
        &json!({
            "test": "offline_synced_replica_keeps_intent_through_restart_and_reconnect",
            "version": env!("CARGO_PKG_VERSION"),
            "sourceSha": std::env::var("LUNA_SOURCE_SHA").ok(),
            "inFlightBatches": in_flight.len(),
            "offlineQueue": { "intentBatches": queued.len(), "stats": offline_stats },
            "restartQueueEqual": true,
            "feed": feed,
            "cascadeSeq": cascade_frame.map(|frame| frame["seq"].clone()),
            "rowsFramesAfterReconnect": frames.len(),
            "expectedHash": hash,
            "serverHash": server_view(&server, &ids).hash(),
            "aHash": public_view(&a, &ids).hash(),
            "bHash": public_view(&b, &ids).hash(),
        }),
    );
    shutdown(b).await;
    shutdown(a).await;
}
