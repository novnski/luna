//! Workspace registry — the client side of the row-table sidebar sync that
//! replaces the Loro workspace doc (docs/registry-sync.md).
//!
//! [`RegistryDoc`] is a local replica of the per-user registry room:
//! - `authoritative` rows — the server's truth, replaced wholesale by `state`/
//!   `rows` frames (the server merges; clients never re-merge server rows);
//! - `pending` op batches — local writes not yet acked, replayed over the
//!   authoritative rows for every read (optimistic overlay), re-pushed on
//!   reconnect (idempotent by strict-`>` clock compare);
//! - an HLC clock stamping every local write.
//!
//! The engine profile fixes a [`RegistryMode`] when the host opens the doc
//! (docs/pending-tasks/zeron-upstream-improvements/zi-02/README.md). A
//! permanently local profile has no server to ack anything: its writes fold
//! straight into the rows and `pending` stays empty. A sync-capable replica —
//! online, offline or edge-less — keeps every write's intent queued; only an
//! unsent same-row update that a later write fully supersedes is retired.
//!
//! The merge function [`apply_op`] mirrors `edge/src/registry-core.ts` 1:1 —
//! the shared test vectors live in both files; change them together. The
//! typed API mirrors the old `WorkspaceDoc` surface so `WorkspaceHost` is a
//! drop-in swap.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use luna_proto::{Chat, ChatConfig, Device, Session, Space};

use crate::schema::DocError;
use crate::workspace::{DeletedSpace, WorkspaceState};

/// Row kinds — the four sidebar tables.
pub const KIND_DEVICES: &str = "devices";
pub const KIND_SPACES: &str = "spaces";
pub const KIND_CHATS: &str = "chats";
pub const KIND_SESSIONS: &str = "sessions";

/// Snapshot row id in the local `DocsStore` for the persisted registry state.
pub const REGISTRY_DOC_ID: &str = "registry1";

// ── HLC ─────────────────────────────────────────────────────────────────────

/// Encode an HLC string: `{ms:013}-{counter:06}-{device}`. Fixed-width zero
/// padding makes lexicographic order = (ms, counter, device) order, and the
/// device suffix makes the order total (two writers can never tie).
pub fn encode_hlc(ms: i64, counter: u32, device: &str) -> String {
    format!("{ms:013}-{counter:06}-{device}")
}

/// `a` strictly newer than `b` (`None` = never written, loses to any).
fn hlc_newer(a: &str, b: Option<&str>) -> bool {
    match b {
        None => true,
        Some(b) => a > b,
    }
}

/// Monotonic HLC source: never emits the same or an earlier clock twice, even
/// across a wall-clock regression or restart (state persists with the doc).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HlcClock {
    last_ms: i64,
    counter: u32,
}

impl HlcClock {
    fn next(&mut self, now_ms: i64, device: &str) -> String {
        if now_ms > self.last_ms {
            self.last_ms = now_ms;
            self.counter = 0;
        } else {
            self.counter += 1;
            if self.counter > 999_999 {
                self.last_ms += 1;
                self.counter = 0;
            }
        }
        encode_hlc(self.last_ms, self.counter, device)
    }
}

// ── rows and ops (wire-compatible with edge/src/registry-core.ts) ───────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryRow {
    pub kind: String,
    pub id: String,
    /// Server seq of the batch that last touched this row (0 locally).
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub deleted: bool,
    /// Tombstone clock — an upsert newer than this revives the row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub del_hlc: Option<String>,
    #[serde(default)]
    pub fields: BTreeMap<String, Value>,
    /// Per-field last-write clocks.
    #[serde(default)]
    pub clocks: BTreeMap<String, String>,
}

impl RegistryRow {
    fn tombstone(kind: &str, id: &str, hlc: String) -> Self {
        Self {
            kind: kind.to_string(),
            id: id.to_string(),
            seq: 0,
            deleted: true,
            del_hlc: Some(hlc),
            fields: BTreeMap::new(),
            clocks: BTreeMap::new(),
        }
    }

    /// The newest clock anywhere on the row (delete-vs-live comparison base).
    fn max_clock(&self) -> Option<&str> {
        let mut max = self.del_hlc.as_deref();
        for clock in self.clocks.values() {
            if max.is_none_or(|m| clock.as_str() > m) {
                max = Some(clock);
            }
        }
        max
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpKind {
    /// Creates, and revives tombstones when newer.
    Upsert,
    /// Never creates or revives ("never invent rows").
    Update,
    /// Tombstones when causally newer than the row.
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowOp {
    pub kind: String,
    pub id: String,
    pub op: OpKind,
    /// Field writes; `Value::Null` deletes the field (still a clocked write).
    /// Absent for deletes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<BTreeMap<String, Value>>,
    /// Clock for every write in `set` without an entry in `clocks`.
    pub hlc: String,
    /// Per-field clock overrides — re-seed pushes carry a row's ORIGINAL
    /// clocks so recovery never coarsens causality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clocks: Option<BTreeMap<String, String>>,
}

impl RowOp {
    fn clock_for<'a>(&'a self, field: &str) -> &'a str {
        self.clocks
            .as_ref()
            .and_then(|c| c.get(field))
            .map_or(self.hlc.as_str(), String::as_str)
    }
}

/// Apply one op to a row — the 1:1 mirror of `applyOp` in
/// `edge/src/registry-core.ts`. Returns the new row (`None` only for an
/// `update` on a missing row) and whether anything changed.
pub fn apply_op(row: Option<&RegistryRow>, op: &RowOp) -> (Option<RegistryRow>, bool) {
    if op.op == OpKind::Delete {
        return match row {
            // Tombstone-on-missing guards against a late create racing the delete.
            None => (
                Some(RegistryRow::tombstone(&op.kind, &op.id, op.hlc.clone())),
                true,
            ),
            Some(row) => {
                let beats = if row.deleted {
                    hlc_newer(&op.hlc, row.del_hlc.as_deref())
                } else {
                    hlc_newer(&op.hlc, row.max_clock())
                };
                if beats {
                    let mut gone = row.clone();
                    gone.deleted = true;
                    gone.del_hlc = Some(op.hlc.clone());
                    gone.fields.clear();
                    gone.clocks.clear();
                    (Some(gone), true)
                } else {
                    (Some(row.clone()), false)
                }
            }
        };
    }

    let mut base = match row {
        None => {
            if op.op == OpKind::Update {
                return (None, false);
            }
            RegistryRow {
                kind: op.kind.clone(),
                id: op.id.clone(),
                seq: 0,
                deleted: false,
                del_hlc: None,
                fields: BTreeMap::new(),
                clocks: BTreeMap::new(),
            }
        }
        Some(row) if row.deleted => {
            if op.op == OpKind::Update || !hlc_newer(&op.hlc, row.del_hlc.as_deref()) {
                return (Some(row.clone()), false);
            }
            // Revival: the tombstone loses wholesale; the upsert's fields are
            // the row.
            RegistryRow {
                kind: row.kind.clone(),
                id: row.id.clone(),
                seq: row.seq,
                deleted: false,
                del_hlc: row.del_hlc.clone(),
                fields: BTreeMap::new(),
                clocks: BTreeMap::new(),
            }
        }
        Some(row) => row.clone(),
    };

    let mut changed = row.is_none() || (row.is_some_and(|r| r.deleted) && !base.deleted);
    if let Some(set) = &op.set {
        for (key, value) in set {
            let clock = op.clock_for(key);
            if !hlc_newer(clock, base.clocks.get(key).map(String::as_str)) {
                continue;
            }
            if value.is_null() {
                base.fields.remove(key);
            } else {
                base.fields.insert(key.clone(), value.clone());
            }
            base.clocks.insert(key.clone(), clock.to_string());
            changed = true;
        }
    }
    if changed {
        base.deleted = false;
        (Some(base), true)
    } else {
        (Some(base), false)
    }
}

/// A row as a re-seed op (server-behind-client recovery): one upsert carrying
/// the row's ORIGINAL per-field clocks, or a delete for tombstones.
pub fn row_to_seed_op(row: &RegistryRow) -> RowOp {
    if row.deleted {
        return RowOp {
            kind: row.kind.clone(),
            id: row.id.clone(),
            op: OpKind::Delete,
            set: None,
            hlc: row
                .del_hlc
                .clone()
                .unwrap_or_else(|| encode_hlc(0, 0, "seed")),
            clocks: None,
        };
    }
    RowOp {
        kind: row.kind.clone(),
        id: row.id.clone(),
        op: OpKind::Upsert,
        set: Some(
            row.fields
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        hlc: row
            .max_clock()
            .map(str::to_string)
            .unwrap_or_else(|| encode_hlc(0, 0, "seed")),
        clocks: Some(row.clocks.clone()),
    }
}

// ── pending batches ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingBatch {
    pub batch: String,
    pub ops: Vec<RowOp>,
    /// True while the batch is in flight on the CURRENT connection (cleared
    /// on disconnect so reconnects re-push). Not persisted meaningfully — a
    /// restart implies a fresh connection.
    #[serde(default, skip_serializing)]
    pub in_flight: bool,
}

// ── profile mode ────────────────────────────────────────────────────────────

/// How a replica retains its own writes. The engine profile supplies it when
/// the host opens the registry; connectivity never infers it — an edge-less
/// synced or development profile is still a replica whose queued intent
/// must reach a server later.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RegistryMode {
    /// Sync-capable: every local write stays a pending batch until a registry
    /// server acknowledges it, however long the replica is offline.
    #[default]
    Replica,
    /// A permanently local profile: no registry server can ever acknowledge a
    /// batch, so writes fold straight into the rows and `pending` stays empty.
    LocalOnly,
}

impl RegistryMode {
    fn is_replica(&self) -> bool {
        *self == Self::Replica
    }
}

/// What [`RegistryDoc::from_bytes_in_mode`] changed so a persisted snapshot
/// matches the opening profile's mode. All zero: the bytes already matched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnapshotReconciliation {
    /// Queued ops folded into rows (an older permanently local snapshot).
    pub folded_ops: usize,
    /// Unsent replica updates retired because a later queued write rewrites
    /// every field they set (a queue persisted before coalescing existed).
    pub coalesced_ops: usize,
    /// Rows re-queued with their original clocks (a local-only snapshot
    /// opened by a sync-capable profile).
    pub requeued_rows: usize,
}

impl SnapshotReconciliation {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Fields whose only meaning is their latest value: no peer, Board or
/// notification path reacts to an intermediate one. Lifecycle fields
/// (`archived`, `settlementOverride`, `removedAt`, `spaceId`, `roomGen`,
/// ownership and lineage) and every `sessions` row stay one op per write, so
/// working → needs-input → working/completed transitions and archive/restore
/// survive an offline queue intact.
const LATEST_VALUE_FIELDS: &[&str] = &[
    "branch",
    "checkoutId",
    "config",
    "cwd",
    "executionDefaults",
    "gitCheckedAt",
    "gitDetected",
    "harnessSessionCwd",
    "harnessSessionId",
    "lastMessageAt",
    "lastMessagePreview",
    "lastSeenAt",
    "manuallyUnread",
    "name",
    "pinnedOrder",
    "sourceContext",
    "title",
];

/// A local field write that a later write of a superset of its fields may
/// replace while both are unsent: an `update` (never creates or revives)
/// stamped with one clock, touching only latest-value fields of a
/// non-session row.
fn coalescable(op: &RowOp) -> bool {
    op.op == OpKind::Update
        && op.clocks.is_none()
        && op.kind != KIND_SESSIONS
        && op.set.as_ref().is_some_and(|set| {
            !set.is_empty()
                && set
                    .keys()
                    .all(|key| LATEST_VALUE_FIELDS.contains(&key.as_str()))
        })
}

// ── the doc ─────────────────────────────────────────────────────────────────

/// What [`RegistryDoc::apply_state`] decided about a `state` frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateOutcome {
    /// Delta applied over existing authoritative rows.
    Delta,
    /// Full state replaced local authoritative rows.
    Replaced,
    /// The server is BEHIND this client (wiped/reset storage): local rows were
    /// kept and a re-seed batch was enqueued. The caller must push pending.
    Reseeded,
}

/// Bump to force every client ONE full resync on its next boot (persisted
/// snapshots below this epoch zero their cursor on load). 1 = the
/// cursor-jump healing (ack/gap fixes below).
const CURRENT_RESYNC_EPOCH: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedState {
    v: u32,
    /// Cursor-integrity epoch: bumping [`CURRENT_RESYNC_EPOCH`] forces every
    /// client one full resync on its first load after upgrading (heals
    /// snapshots whose cursor jumped past unapplied rows). Additive —
    /// pre-epoch snapshots default to 0.
    #[serde(default)]
    resync_epoch: u32,
    /// Retention mode, restored by every reconstruction (rollback included).
    /// Additive and omitted for replicas, so a replica snapshot keeps its
    /// exact earlier bytes and an older build reads a local-only snapshot as
    /// rows with an empty queue.
    #[serde(default, skip_serializing_if = "RegistryMode::is_replica")]
    mode: RegistryMode,
    device_id: String,
    server_seq: u64,
    gc_floor: u64,
    clock: HlcClock,
    rows: Vec<RegistryRow>,
    pending: Vec<PendingBatch>,
}

/// The local registry replica. Pure data — no I/O, no async; the transport
/// (`luna_sync::RegistryClient`) and the engine host drive it under a lock.
pub struct RegistryDoc {
    device_id: String,
    /// kind → id → row (server truth).
    authoritative: HashMap<String, HashMap<String, RegistryRow>>,
    server_seq: u64,
    gc_floor: u64,
    clock: HlcClock,
    pending: Vec<PendingBatch>,
    /// Fixed at construction: the profile's capability on open, or the
    /// persisted marker on an exact reconstruction. See [`RegistryMode`].
    mode: RegistryMode,
    /// Bumped on every mutation (local or applied) — the engine host converts
    /// this into watch-channel publishes and snapshot debounces.
    generation: u64,
}

impl RegistryDoc {
    pub fn new(device_id: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            authoritative: HashMap::new(),
            server_seq: 0,
            gc_floor: 0,
            clock: HlcClock::default(),
            pending: Vec::new(),
            mode: RegistryMode::Replica,
            generation: 0,
        }
    }

    /// An empty doc in the profile's `mode` (first boot, legacy seed).
    pub fn new_in_mode(device_id: impl Into<String>, mode: RegistryMode) -> Self {
        let mut doc = Self::new(device_id);
        doc.mode = mode;
        doc
    }

    pub fn mode(&self) -> RegistryMode {
        self.mode
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Sync cursor: the last server seq this replica has fully applied.
    pub fn cursor(&self) -> u64 {
        self.server_seq
    }

    /// Monotonic local change counter (any mutation bumps it).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    // ── persistence ─────────────────────────────────────────────────────────

    // (see PersistedState::resync_epoch)

    pub fn to_bytes(&self) -> Result<Vec<u8>, DocError> {
        let rows = self
            .authoritative
            .values()
            .flat_map(|m| m.values().cloned())
            .collect();
        let state = PersistedState {
            v: 1,
            resync_epoch: CURRENT_RESYNC_EPOCH,
            mode: self.mode,
            device_id: self.device_id.clone(),
            server_seq: self.server_seq,
            gc_floor: self.gc_floor,
            clock: self.clock.clone(),
            rows,
            pending: self.pending.clone(),
        };
        Ok(serde_json::to_vec(&state)?)
    }

    /// Exact reconstruction (restart readers, guarded rollback, backup and
    /// import). The persisted mode comes back with the rows, so a rolled-back
    /// permanently local doc keeps folding instead of queueing again.
    pub fn from_bytes(bytes: &[u8], device_id: &str) -> Result<Self, DocError> {
        let mut doc = Self::decode(bytes, device_id)?;
        // A local-only snapshot never persists a queue. Fold a foreign one at
        // once: a folded write must never be replayed under an older op.
        if doc.mode == RegistryMode::LocalOnly {
            doc.fold_pending();
        }
        Ok(doc)
    }

    /// Open a persisted snapshot for a profile whose `mode` the engine
    /// supplies, reconciling the bytes with it once:
    /// - a permanently local profile folds any queued ops into the rows in
    ///   their original order (same merge, HLCs and tombstones as the
    ///   overlay it replaces);
    /// - a replica retires unsent updates a later queued write supersedes
    ///   (the write-time rule, see `drop_superseded_unsent`);
    /// - a replica opening a local-only snapshot re-queues every row with its
    ///   original clocks, so a server merges that intent instead of
    ///   replacing it.
    ///
    /// Pure: the caller persists the result through its guarded save. Until
    /// that commits the stored bytes are unchanged and the next open repeats
    /// the same deterministic reconciliation.
    pub fn from_bytes_in_mode(
        bytes: &[u8],
        device_id: &str,
        mode: RegistryMode,
    ) -> Result<(Self, SnapshotReconciliation), DocError> {
        let mut doc = Self::decode(bytes, device_id)?;
        let mut report = SnapshotReconciliation::default();
        match (doc.mode, mode) {
            (_, RegistryMode::LocalOnly) => {
                doc.mode = RegistryMode::LocalOnly;
                report.folded_ops = doc.fold_pending();
            }
            (RegistryMode::LocalOnly, RegistryMode::Replica) => {
                // No server has seen these folded writes (the server-behind
                // recovery of `apply_state`).
                doc.mode = RegistryMode::Replica;
                let seed: Vec<RowOp> = doc
                    .authoritative
                    .values()
                    .flat_map(|rows| rows.values())
                    .map(row_to_seed_op)
                    .collect();
                report.requeued_rows = seed.len();
                doc.enqueue_ops(seed);
            }
            (RegistryMode::Replica, RegistryMode::Replica) => {
                report.coalesced_ops = doc.compact_unsent();
            }
        }
        Ok((doc, report))
    }

    fn decode(bytes: &[u8], device_id: &str) -> Result<Self, DocError> {
        let state: PersistedState = serde_json::from_slice(bytes)?;
        if state.v != 1 {
            return Err(DocError::Schema(format!(
                "unknown registry snapshot version {}",
                state.v
            )));
        }
        let mut doc = Self::new(device_id);
        // One-shot healing resync: snapshots from before a cursor-integrity
        // fix may have a cursor that JUMPED past rows this replica never
        // applied (the ack/gap bugs above) — those rows are invisible and no
        // amount of ordinary syncing refetches them. Zeroing the cursor once
        // forces the next hello/pull to return full state; local rows are
        // kept (and local-only ones re-seed through apply_state's full path).
        if state.resync_epoch < CURRENT_RESYNC_EPOCH {
            doc.server_seq = 0;
        } else {
            doc.server_seq = state.server_seq;
        }
        doc.gc_floor = state.gc_floor;
        doc.clock = state.clock;
        doc.mode = state.mode;
        doc.pending = state.pending;
        for row in state.rows {
            doc.authoritative
                .entry(row.kind.clone())
                .or_default()
                .insert(row.id.clone(), row);
        }
        Ok(doc)
    }

    // ── server frames ───────────────────────────────────────────────────────

    /// Apply a hello `state` frame. See [`StateOutcome`] for the three shapes.
    pub fn apply_state(
        &mut self,
        seq: u64,
        full: bool,
        gc_floor: u64,
        rows: Vec<RegistryRow>,
    ) -> StateOutcome {
        self.generation += 1;
        if !full {
            for row in rows {
                self.put_authoritative(row);
            }
            self.server_seq = seq;
            self.gc_floor = gc_floor;
            return StateOutcome::Delta;
        }
        if seq < self.server_seq {
            // The server lost state (reset/wipe). Local rows are the only
            // copy: keep them and re-seed the server with ORIGINAL clocks.
            let seed: Vec<RowOp> = self
                .authoritative
                .values()
                .flat_map(|m| m.values())
                .map(row_to_seed_op)
                .collect();
            for row in rows {
                self.put_authoritative(row);
            }
            self.server_seq = seq;
            self.gc_floor = gc_floor;
            if !seed.is_empty() {
                self.enqueue_ops(seed);
            }
            return StateOutcome::Reseeded;
        }
        // Full replace — but local-only rows the server never saw (e.g. it
        // GC-jumped our cursor while we held unseeded rows) re-seed too.
        let mut incoming: HashMap<String, HashMap<String, RegistryRow>> = HashMap::new();
        for row in rows {
            incoming
                .entry(row.kind.clone())
                .or_default()
                .insert(row.id.clone(), row);
        }
        let mut seed: Vec<RowOp> = Vec::new();
        for (kind, by_id) in &self.authoritative {
            for (id, row) in by_id {
                if !incoming.get(kind).is_some_and(|m| m.contains_key(id)) {
                    seed.push(row_to_seed_op(row));
                }
            }
        }
        self.authoritative = incoming;
        self.server_seq = seq;
        self.gc_floor = gc_floor;
        if seed.is_empty() {
            StateOutcome::Replaced
        } else {
            self.enqueue_ops(seed);
            StateOutcome::Reseeded
        }
    }

    /// Apply a `rows` broadcast (merged truth for the touched rows).
    /// Apply one broadcast batch. Returns `false` on a SEQ GAP — frames
    /// between the cursor and this batch were missed (a hibernating DO, a
    /// half-dead socket): the rows still apply (fresh data), but the cursor
    /// holds so the caller can resync (reconnect → hello/pull from the true
    /// cursor backfills the window). Advancing across a gap made the missed
    /// rows permanently invisible.
    #[must_use]
    pub fn apply_rows(&mut self, seq: u64, rows: Vec<RegistryRow>) -> bool {
        self.generation += 1;
        for row in rows {
            self.put_authoritative(row);
        }
        if seq > self.server_seq + 1 {
            return false;
        }
        if seq > self.server_seq {
            self.server_seq = seq;
        }
        true
    }

    /// Retire an acked batch; returns whether it existed.
    ///
    /// Deliberately does NOT advance the sync cursor: the ack's `seq` is OUR
    /// batch's position, and other devices' batches may sit between the
    /// cursor and it. Jumping the cursor skipped those forever — on the
    /// HTTPS transport the very next pull used the jumped cursor, so a
    /// device that pushed anything (a seen-marker was enough) while behind
    /// permanently missed every row its peers wrote since its last sync
    /// (field incident: new sessions never appeared in a laptop's sidebar
    /// while updates to already-known rows kept flowing). The cursor only
    /// advances with actual row payloads (state/pull/contiguous broadcasts);
    /// re-pulling our own batch is an idempotent LWW no-op.
    pub fn ack_batch(&mut self, batch: &str, _seq: u64) -> bool {
        let before = self.pending.len();
        self.pending.retain(|b| b.batch != batch);
        if self.pending.len() != before {
            self.generation += 1;
            true
        } else {
            false
        }
    }

    /// Batches to push: everything not already in flight on this connection.
    pub fn take_pushable(&mut self) -> Vec<PendingBatch> {
        let mut out = Vec::new();
        for batch in &mut self.pending {
            if !batch.in_flight {
                batch.in_flight = true;
                out.push(batch.clone());
            }
        }
        out
    }

    /// Connection dropped: everything unacked becomes pushable again.
    pub fn mark_disconnected(&mut self) {
        for batch in &mut self.pending {
            batch.in_flight = false;
        }
    }

    fn put_authoritative(&mut self, row: RegistryRow) {
        self.authoritative
            .entry(row.kind.clone())
            .or_default()
            .insert(row.id.clone(), row);
    }

    // ── local writes ────────────────────────────────────────────────────────

    fn now_ms() -> i64 {
        Utc::now().timestamp_millis()
    }

    fn next_hlc(&mut self) -> String {
        let device = self.device_id.clone();
        self.clock.next(Self::now_ms(), &device)
    }

    fn enqueue_ops(&mut self, mut ops: Vec<RowOp>) {
        if ops.is_empty() {
            return;
        }
        if self.mode == RegistryMode::LocalOnly {
            // No server will ever ack these: the rows ARE the local truth.
            self.generation += 1;
            for op in &ops {
                self.fold_op(op);
            }
            return;
        }
        // The registry room rejects any batch over its op cap (500), and a
        // rejected batch is a PERMANENT wedge: error frames carry no batch
        // id, so the client can never retire it — it replays and fails on
        // every reconnect, and every write queued behind it (session-status
        // rows included) never propagates again. `delete_space` on a
        // ≥250-chat space minted exactly such a batch. Chunk here so no
        // writer can ever produce one; chunks apply in order, each
        // atomically (only cross-chunk atomicity is given up).
        const MAX_OPS_PER_BATCH: usize = 400;
        self.generation += 1;
        while !ops.is_empty() {
            let tail = if ops.len() > MAX_OPS_PER_BATCH {
                ops.split_off(MAX_OPS_PER_BATCH)
            } else {
                Vec::new()
            };
            // Batch ids only need device-lifetime uniqueness; the HLC of a
            // fresh tick provides exactly that without a rng dependency.
            let batch = format!("b-{}", self.next_hlc());
            self.pending.push(PendingBatch {
                batch,
                ops,
                in_flight: false,
            });
            ops = tail;
        }
    }

    fn write(&mut self, kind: &str, id: &str, op: OpKind, set: BTreeMap<String, Value>) {
        let hlc = self.next_hlc();
        let op = RowOp {
            kind: kind.to_string(),
            id: id.to_string(),
            op,
            set: Some(set),
            hlc,
            clocks: None,
        };
        self.drop_superseded_unsent(&op);
        self.enqueue_ops(vec![op]);
    }

    /// Apply one op straight to its authoritative row — the step the overlay
    /// replays, made permanent (permanently local profiles only).
    fn fold_op(&mut self, op: &RowOp) {
        let row = self
            .authoritative
            .get(&op.kind)
            .and_then(|rows| rows.get(&op.id));
        if let (Some(next), true) = apply_op(row, op) {
            self.put_authoritative(next);
        }
    }

    /// Fold the whole queue in its original order; returns the ops folded.
    fn fold_pending(&mut self) -> usize {
        let pending = std::mem::take(&mut self.pending);
        let mut folded = 0;
        for batch in &pending {
            for op in &batch.ops {
                self.fold_op(op);
                folded += 1;
            }
        }
        if folded > 0 {
            self.generation += 1;
        }
        folded
    }

    /// Retire unsent single-op updates that `next` makes redundant, before
    /// `next` is queued (sync-capable replicas only). Returns how many.
    ///
    /// Only the contiguous run of unsent single-op [`coalescable`] updates to
    /// `next`'s own row at the tail of the queue is examined, and only
    /// batches whose every field `next` rewrites with a strictly newer clock
    /// are dropped. So:
    /// - for any server row state (live, tombstoned, missing, concurrently
    ///   edited) the merged row is unchanged: per field, the newer write wins
    ///   wherever the older one would have, and an update never decides
    ///   existence;
    /// - no op moves across another row or across a session, lifecycle,
    ///   deletion or multi-op batch, so chat labels stay ordered against
    ///   session transitions and cascades stay whole;
    /// - in-flight batches keep their ids and content, so an ack always
    ///   retires exactly what was pushed. A dropped batch that a dead
    ///   connection already delivered is harmless: `next` supersedes it on
    ///   the server too, and `next` is queued as a fresh batch.
    ///
    /// Retaining an op is always safe; anything outside these rules stays.
    fn drop_superseded_unsent(&mut self, next: &RowOp) -> usize {
        if self.mode != RegistryMode::Replica || !coalescable(next) {
            return 0;
        }
        let Some(fields) = next.set.as_ref() else {
            return 0;
        };
        let same_row = |batch: &PendingBatch| {
            !batch.in_flight
                && matches!(
                    batch.ops.as_slice(),
                    [op] if op.kind == next.kind && op.id == next.id && coalescable(op)
                )
        };
        let start = self
            .pending
            .iter()
            .rposition(|batch| !same_row(batch))
            .map_or(0, |index| index + 1);
        if start == self.pending.len() {
            return 0;
        }
        let before = self.pending.len();
        let run = self.pending.split_off(start);
        self.pending.extend(run.into_iter().filter(|batch| {
            let op = &batch.ops[0];
            let superseded = hlc_newer(&next.hlc, Some(op.hlc.as_str()))
                && op
                    .set
                    .as_ref()
                    .is_some_and(|set| set.keys().all(|key| fields.contains_key(key)));
            !superseded
        }));
        let dropped = before - self.pending.len();
        if dropped > 0 {
            self.generation += 1;
        }
        dropped
    }

    /// The write-time rule replayed over a loaded queue (nothing is in flight
    /// after a load): shrinks queues persisted before coalescing existed and
    /// is a no-op on queues it already maintained. Kept batches keep their
    /// ids. Returns the ops dropped.
    fn compact_unsent(&mut self) -> usize {
        let queued = std::mem::take(&mut self.pending);
        let mut dropped = 0;
        for batch in queued {
            if !batch.in_flight
                && let [op] = batch.ops.as_slice()
            {
                let op = op.clone();
                dropped += self.drop_superseded_unsent(&op);
            }
            self.pending.push(batch);
        }
        dropped
    }

    fn delete_row_ops(&mut self, keys: &[(&str, &str)]) {
        let hlc = self.next_hlc();
        let ops = keys
            .iter()
            .map(|(kind, id)| RowOp {
                kind: (*kind).to_string(),
                id: (*id).to_string(),
                op: OpKind::Delete,
                set: None,
                hlc: hlc.clone(),
                clocks: None,
            })
            .collect();
        self.enqueue_ops(ops);
    }

    // ── overlay reads ───────────────────────────────────────────────────────

    /// The row as this device should display it: authoritative + pending ops.
    fn overlay_row(&self, kind: &str, id: &str) -> Option<RegistryRow> {
        let mut row = self
            .authoritative
            .get(kind)
            .and_then(|m| m.get(id))
            .cloned();
        for batch in &self.pending {
            for op in &batch.ops {
                if op.kind == kind && op.id == id {
                    let (next, _) = apply_op(row.as_ref(), op);
                    if let Some(next) = next {
                        row = Some(next);
                    }
                }
            }
        }
        row.filter(|r| !r.deleted)
    }

    /// All live rows of `kind`, overlay applied.
    fn overlay_rows(&self, kind: &str) -> Vec<RegistryRow> {
        let mut ids: Vec<String> = self
            .authoritative
            .get(kind)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        for batch in &self.pending {
            for op in &batch.ops {
                if op.kind == kind && !ids.iter().any(|id| id == &op.id) {
                    ids.push(op.id.clone());
                }
            }
        }
        ids.iter()
            .filter_map(|id| self.overlay_row(kind, id))
            .collect()
    }

    fn read_kind<T: serde::de::DeserializeOwned>(&self, kind: &str) -> Vec<T> {
        let mut out = Vec::new();
        for row in self.overlay_rows(kind) {
            let value = Value::Object(row.fields.clone().into_iter().collect());
            match serde_json::from_value::<T>(value) {
                Ok(parsed) => out.push(parsed),
                Err(err) => {
                    tracing::warn!(kind, row = %row.id, error = %err, "skipping malformed registry row");
                }
            }
        }
        out
    }

    fn row_exists(&self, kind: &str, id: &str) -> bool {
        self.overlay_row(kind, id).is_some()
    }

    // ── typed API (the WorkspaceDoc surface) ────────────────────────────────

    /// Upsert a full device row (writer discipline: callers pass their OWN device).
    pub fn upsert_device(&mut self, device: &Device) -> Result<(), DocError> {
        let set = fields([
            ("id", json!(device.id)),
            ("name", json!(device.name)),
            ("platform", json!(device.platform)),
            ("lastSeenAt", opt_ms(device.last_seen_at)),
            ("createdAt", opt_ms(device.created_at)),
            ("version", opt_str(device.version.as_deref())),
            ("capabilities", json!(device.capabilities)),
        ]);
        self.write(KIND_DEVICES, &device.id.clone(), OpKind::Upsert, set);
        Ok(())
    }

    /// LWW rename (settings UI; any device may write). `false` when no such row.
    pub fn rename_device(&mut self, device_id: &str, name: &str) -> Result<bool, DocError> {
        if !self.row_exists(KIND_DEVICES, device_id) {
            return Ok(false);
        }
        self.write(
            KIND_DEVICES,
            device_id,
            OpKind::Update,
            fields([("name", json!(name))]),
        );
        Ok(true)
    }

    /// Stamp `lastSeenAt` on an existing device row (boot/shutdown only —
    /// periodic liveness rides presence frames, never rows).
    pub fn set_device_last_seen(
        &mut self,
        device_id: &str,
        at: DateTime<Utc>,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_DEVICES, device_id) {
            return Ok(false);
        }
        self.write(
            KIND_DEVICES,
            device_id,
            OpKind::Update,
            fields([("lastSeenAt", json!(at.timestamp_millis()))]),
        );
        Ok(true)
    }

    pub fn read_devices(&self) -> Result<Vec<Device>, DocError> {
        let mut devices: Vec<Device> = self
            .read_kind::<crate::workspace::RawDevice>(KIND_DEVICES)
            .into_iter()
            .map(Device::from)
            .collect();
        devices.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(devices)
    }

    // ── spaces ──────────────────────────────────────────────────────────────

    pub fn upsert_space(&mut self, space: &Space) -> Result<(), DocError> {
        let set = fields([
            ("id", json!(space.id)),
            ("deviceId", json!(space.device_id)),
            ("path", json!(space.path)),
            ("name", opt_str(space.name.as_deref())),
            ("gitDetected", json!(space.git_detected)),
            ("gitCheckedAt", opt_ms(space.git_checked_at)),
            ("checkoutId", opt_str(space.checkout_id.as_deref())),
            (
                "executionDefaults",
                serde_json::to_value(&space.execution_defaults)?,
            ),
            ("createdAt", json!(space.created_at.timestamp_millis())),
            ("removedAt", opt_ms(space.removed_at)),
        ]);
        self.write(KIND_SPACES, &space.id.clone(), OpKind::Upsert, set);
        Ok(())
    }

    pub fn space(&self, space_id: &str) -> Result<Option<Space>, DocError> {
        Ok(self
            .overlay_row(KIND_SPACES, space_id)
            .and_then(|row| row_to::<crate::workspace::RawSpace>(&row))
            .map(Space::from))
    }

    pub fn read_spaces(&self) -> Result<Vec<Space>, DocError> {
        let mut spaces: Vec<Space> = self
            .read_kind::<crate::workspace::RawSpace>(KIND_SPACES)
            .into_iter()
            .map(Space::from)
            .collect();
        spaces.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(spaces)
    }

    /// LWW display-name set from any device; `None` clears back to the derived
    /// name (basename of path). `false` when no such row.
    pub fn rename_space(&mut self, space_id: &str, name: Option<&str>) -> Result<bool, DocError> {
        if !self.row_exists(KIND_SPACES, space_id) {
            return Ok(false);
        }
        self.write(
            KIND_SPACES,
            space_id,
            OpKind::Update,
            fields([("name", opt_str(name))]),
        );
        Ok(true)
    }

    /// Targeted update leaves sibling project settings and task rows untouched.
    pub fn set_project_execution_defaults(
        &mut self,
        space_id: &str,
        defaults: &luna_proto::ProjectExecutionDefaults,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_SPACES, space_id) {
            return Ok(false);
        }
        self.write(
            KIND_SPACES,
            space_id,
            OpKind::Update,
            fields([("executionDefaults", serde_json::to_value(defaults)?)]),
        );
        Ok(true)
    }

    /// Hide (`Some`) or restore (`None`) a project without touching its
    /// sessions. A targeted field update, so a concurrent rename or git stamp
    /// from another device survives. `false` when no such row.
    pub fn set_space_removed(
        &mut self,
        space_id: &str,
        removed_at: Option<DateTime<Utc>>,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_SPACES, space_id) {
            return Ok(false);
        }
        self.write(
            KIND_SPACES,
            space_id,
            OpKind::Update,
            fields([("removedAt", opt_ms(removed_at))]),
        );
        Ok(true)
    }

    /// Owner-stamped git presence for the space folder (ownership is asserted
    /// by the engine layer, this is mechanism only).
    pub fn set_space_git(
        &mut self,
        space_id: &str,
        detected: bool,
        checkout_id: Option<&str>,
        checked_at: DateTime<Utc>,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_SPACES, space_id) {
            return Ok(false);
        }
        self.write(
            KIND_SPACES,
            space_id,
            OpKind::Update,
            fields([
                ("gitDetected", json!(detected)),
                ("checkoutId", opt_str(checkout_id)),
                ("gitCheckedAt", json!(checked_at.timestamp_millis())),
            ]),
        );
        Ok(true)
    }

    /// Hard-delete a space and cascade to its chats: one batch tombstones the
    /// space row and every chat/session row whose `spaceId` matches — the
    /// server applies the batch atomically. Returns the removed chat ids so
    /// the engine can drop local state.
    pub fn delete_space(&mut self, space_id: &str) -> Result<DeletedSpace, DocError> {
        let existed = self.row_exists(KIND_SPACES, space_id);
        let chat_ids: Vec<String> = self
            .read_chats()?
            .into_iter()
            .filter(|c| c.space_id.as_deref() == Some(space_id))
            .map(|c| c.id)
            .collect();
        let mut keys: Vec<(&str, &str)> = Vec::with_capacity(chat_ids.len() * 2 + 1);
        for chat_id in &chat_ids {
            keys.push((KIND_CHATS, chat_id));
            keys.push((KIND_SESSIONS, chat_id));
        }
        keys.push((KIND_SPACES, space_id));
        self.delete_row_ops(&keys);
        Ok(DeletedSpace { existed, chat_ids })
    }

    // ── chats ───────────────────────────────────────────────────────────────

    pub fn upsert_chat(&mut self, chat: &Chat) -> Result<(), DocError> {
        self.upsert_chat_with_side_parent(chat, None)
    }

    /// One row operation carries ownership and hidden visibility together,
    /// so peers never observe a side child as a main-sidebar task.
    pub fn upsert_side_chat(&mut self, chat: &Chat, parent_id: &str) -> Result<(), DocError> {
        self.upsert_chat_with_side_parent(chat, Some(parent_id))
    }

    pub fn side_chat_parent(&self, chat_id: &str) -> Option<String> {
        let row = self.overlay_row(KIND_CHATS, chat_id)?;
        if row.deleted {
            return None;
        }
        row.fields
            .get("sideChatParentId")?
            .as_str()
            .filter(|parent| !parent.is_empty())
            .map(str::to_owned)
    }

    fn upsert_chat_with_side_parent(
        &mut self,
        chat: &Chat,
        side_parent: Option<&str>,
    ) -> Result<(), DocError> {
        let config = match &chat.config {
            Some(config) => serde_json::to_value(config)?,
            None => Value::Null,
        };
        let mut set = fields([
            ("id", json!(chat.id)),
            ("deviceId", json!(chat.device_id)),
            ("title", opt_str(chat.title.as_deref())),
            ("archived", json!(chat.archived)),
            (
                "settlementOverride",
                chat.settlement_override
                    .as_ref()
                    .map(serde_json::to_value)
                    .transpose()?
                    .unwrap_or(Value::Null),
            ),
            (
                "pinnedOrder",
                chat.pinned_order
                    .as_deref()
                    .map(|order| json!(order))
                    .unwrap_or(Value::Null),
            ),
            ("cwd", opt_str(chat.cwd.as_deref())),
            ("branch", opt_str(chat.branch.as_deref())),
            ("checkoutId", opt_str(chat.checkout_id.as_deref())),
            (
                "sourceContext",
                chat.source_context
                    .as_ref()
                    .map(serde_json::to_value)
                    .transpose()?
                    .unwrap_or(Value::Null),
            ),
            ("config", config),
            (
                "lastMessagePreview",
                opt_str(chat.last_message_preview.as_deref()),
            ),
            ("lastMessageAt", opt_ms(chat.last_message_at)),
            ("createdAt", json!(chat.created_at.timestamp_millis())),
            (
                "harnessSessionId",
                opt_str(chat.harness_session_id.as_deref()),
            ),
            (
                "harnessSessionCwd",
                opt_str(chat.harness_session_cwd.as_deref()),
            ),
            ("spaceId", opt_str(chat.space_id.as_deref())),
            ("lastSeenAt", opt_ms(chat.last_seen_at)),
            ("manuallyUnread", json!(chat.manually_unread)),
            (
                "roomGen",
                chat.room_gen.map(|g| json!(g)).unwrap_or(Value::Null),
            ),
        ]);
        if let Some(parent) = side_parent {
            set.insert("sideChatParentId".into(), json!(parent));
        }
        // Lineage is set once at creation and never cleared. Omitting the key
        // for ordinary rows keeps a full-row upsert built without it (or from
        // an older peer) from clocking a delete over a child's lineage/outbox.
        if let Some(child) = &chat.child_thread {
            set.insert("childThread".into(), serde_json::to_value(child)?);
        }
        self.write(KIND_CHATS, &chat.id.clone(), OpKind::Upsert, set);
        Ok(())
    }

    /// Claim-on-first-command row: ONLY the fields the claim actually knows
    /// (identity, cwd, space). Absent fields carry no clocked write, so the
    /// real `createChat` upsert — racing behind on the registry channel with
    /// older clocks — still lands its `config`/`title` under per-field LWW
    /// instead of losing them to `Null` deletes.
    pub fn claim_chat(
        &mut self,
        chat_id: &str,
        cwd: Option<&str>,
        space_id: Option<&str>,
        created_at: DateTime<Utc>,
    ) {
        let mut set = fields([
            ("id", json!(chat_id)),
            ("deviceId", json!(self.device_id())),
            ("createdAt", json!(created_at.timestamp_millis())),
        ]);
        if let Some(cwd) = cwd {
            set.insert("cwd".into(), json!(cwd));
        }
        if let Some(space_id) = space_id {
            set.insert("spaceId".into(), json!(space_id));
        }
        self.write(KIND_CHATS, chat_id, OpKind::Upsert, set);
    }

    /// Synced seen marker (LWW) with a monotonic guard: no write when the
    /// stored stamp is already >= `at`.
    pub fn set_chat_seen(&mut self, chat_id: &str, at: DateTime<Utc>) -> Result<bool, DocError> {
        let Some(row) = self.overlay_row(KIND_CHATS, chat_id) else {
            return Ok(false);
        };
        let current = row.fields.get("lastSeenAt").and_then(Value::as_i64);
        if current.is_some_and(|ms| ms >= at.timestamp_millis()) {
            return Ok(true);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("lastSeenAt", json!(at.timestamp_millis()))]),
        );
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("manuallyUnread", json!(false))]),
        );
        Ok(true)
    }

    pub fn set_chat_unread(&mut self, chat_id: &str, unread: bool) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("manuallyUnread", json!(unread))]),
        );
        Ok(true)
    }

    pub fn set_chat_pinned_order(
        &mut self,
        chat_id: &str,
        order: Option<&str>,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([(
                "pinnedOrder",
                order.map(|value| json!(value)).unwrap_or(Value::Null),
            )]),
        );
        Ok(true)
    }

    pub fn chat(&self, chat_id: &str) -> Result<Option<Chat>, DocError> {
        Ok(self
            .overlay_row(KIND_CHATS, chat_id)
            .and_then(|row| row_to::<crate::workspace::RawChat>(&row))
            .map(Chat::from))
    }

    pub fn read_chats(&self) -> Result<Vec<Chat>, DocError> {
        let mut chats: Vec<Chat> = self
            .read_kind::<crate::workspace::RawChat>(KIND_CHATS)
            .into_iter()
            .map(Chat::from)
            .collect();
        chats.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(chats)
    }

    pub fn rename_chat(&mut self, chat_id: &str, title: &str) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("title", json!(title))]),
        );
        Ok(true)
    }

    pub fn set_chat_archived(&mut self, chat_id: &str, archived: bool) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        let mut set = fields([("archived", json!(archived))]);
        if archived {
            set.insert("pinnedOrder".into(), Value::Null);
        }
        self.write(KIND_CHATS, chat_id, OpKind::Update, set);
        Ok(true)
    }

    pub fn set_chat_space(&mut self, chat_id: &str, space_id: &str) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) || !self.row_exists(KIND_SPACES, space_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("spaceId", json!(space_id))]),
        );
        Ok(true)
    }

    /// Flip the chat's sync room generation (docs/chat2-sync.md M2). The
    /// host calls this in the same breath as seeding the chat2 checkpoint;
    /// LWW per-field like every registry write, so the flip is per-chat and
    /// instantly revertible by writing 1 back.
    pub fn set_chat_room_gen(&mut self, chat_id: &str, room_gen: u32) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("roomGen", json!(room_gen))]),
        );
        Ok(true)
    }

    pub fn set_chat_branch(&mut self, chat_id: &str, branch: &str) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("branch", json!(branch))]),
        );
        Ok(true)
    }

    /// Rename the branch checked out at `cwd` in place (the automatic title
    /// rename). The ref keeps its commits and checkout, so a source context
    /// stamped on that folder under the old name still describes this
    /// conversation and takes the new name in the same write. A context
    /// observed on another folder or branch is left for the next dispatch.
    pub fn rename_chat_branch(
        &mut self,
        chat_id: &str,
        cwd: &str,
        from: &str,
        to: &str,
    ) -> Result<bool, DocError> {
        let Some(chat) = self.chat(chat_id)? else {
            return Ok(false);
        };
        let mut set = fields([("branch", json!(to))]);
        if let Some(mut source) = chat.source_context
            && source.cwd == cwd
            && source.branch == from
        {
            source.branch = to.to_string();
            set.insert("sourceContext".into(), serde_json::to_value(&source)?);
        }
        self.write(KIND_CHATS, chat_id, OpKind::Update, set);
        Ok(true)
    }

    pub fn set_chat_source_context(
        &mut self,
        chat_id: &str,
        context: &luna_proto::ConversationSourceContext,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([
                ("sourceContext", serde_json::to_value(context)?),
                ("branch", json!(context.branch)),
                ("checkoutId", json!(context.checkout_id)),
            ]),
        );
        Ok(true)
    }

    /// Retarget the chat onto another folder — the mid-session "switch to an
    /// existing worktree" move. Harness resume is cwd-scoped, so the next run
    /// in the new folder starts a fresh harness conversation by design.
    pub fn set_chat_cwd(&mut self, chat_id: &str, cwd: &str) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("cwd", json!(cwd)), ("checkoutId", serde_json::Value::Null)]),
        );
        Ok(true)
    }

    pub fn set_chat_checkout(
        &mut self,
        chat_id: &str,
        checkout_id: &str,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("checkoutId", json!(checkout_id))]),
        );
        Ok(true)
    }

    pub fn set_chat_config(
        &mut self,
        chat_id: &str,
        config: &ChatConfig,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        let value = serde_json::to_value(config)?;
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("config", value)]),
        );
        Ok(true)
    }

    /// Host-side resume continuity. An empty `session_id` is the explicit
    /// "do not resume" tombstone written after a harness rejects a resume.
    pub fn set_chat_harness_session(
        &mut self,
        chat_id: &str,
        session_id: &str,
        cwd: &str,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([
                ("harnessSessionId", json!(session_id)),
                ("harnessSessionCwd", json!(cwd)),
            ]),
        );
        Ok(true)
    }

    /// Host-side sidebar freshness: preview + timestamp of the latest message.
    pub fn set_chat_last_message(
        &mut self,
        chat_id: &str,
        preview: &str,
        at: DateTime<Utc>,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([
                ("lastMessagePreview", json!(preview)),
                ("lastMessageAt", json!(at.timestamp_millis())),
            ]),
        );
        Ok(true)
    }

    pub fn set_chat_settlement_override(
        &mut self,
        chat_id: &str,
        settlement: &luna_proto::ChatSettlementOverride,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("settlementOverride", serde_json::to_value(settlement)?)]),
        );
        Ok(true)
    }

    /// Tombstone: delete the chat row (and its session-status row). The
    /// per-chat session doc remains — this removes the index entry only.
    pub fn delete_chat(&mut self, chat_id: &str) -> Result<bool, DocError> {
        let existed = self.row_exists(KIND_CHATS, chat_id);
        self.delete_row_ops(&[(KIND_CHATS, chat_id), (KIND_SESSIONS, chat_id)]);
        Ok(existed)
    }

    // ── sessions ────────────────────────────────────────────────────────────

    /// Upsert a session-status row (writer discipline: each device writes only
    /// its own runs' rows). Staleness is checked client-side via `updatedAt`.
    pub fn upsert_session(&mut self, session: &Session) -> Result<(), DocError> {
        let set = fields([
            ("chatId", json!(session.chat_id)),
            ("deviceId", json!(session.device_id)),
            ("status", serde_json::to_value(session.status)?),
            ("lastCompletedTurn", json!(session.last_completed_turn)),
            ("startedAt", opt_ms(session.started_at)),
            ("updatedAt", json!(session.updated_at.timestamp_millis())),
        ]);
        self.write(KIND_SESSIONS, &session.chat_id.clone(), OpKind::Upsert, set);
        Ok(())
    }

    pub fn read_sessions(&self) -> Result<Vec<Session>, DocError> {
        let mut sessions: Vec<Session> = self
            .read_kind::<crate::workspace::RawSession>(KIND_SESSIONS)
            .into_iter()
            .map(Session::from)
            .collect();
        sessions.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
        Ok(sessions)
    }

    // ── whole-doc read ──────────────────────────────────────────────────────

    pub fn read_all(&self) -> Result<WorkspaceState, DocError> {
        Ok(WorkspaceState {
            devices: self.read_devices()?,
            spaces: self.read_spaces()?,
            chats: self.read_chats()?,
            sessions: self.read_sessions()?,
        })
    }

    // ── migration ───────────────────────────────────────────────────────────

    /// Seed from the legacy Loro workspace doc's materialized state (first
    /// boot after the update). Every row becomes a pending upsert whose HLC
    /// derives from the row's own newest timestamp — historical, so any
    /// genuinely newer live write beats the migrated value; identical across
    /// devices, so N devices seeding the same converged doc is idempotent
    /// (equal values, deterministic device tie-break).
    pub fn seed_from_workspace(&mut self, state: &WorkspaceState) -> Result<usize, DocError> {
        let mut ops: Vec<RowOp> = Vec::new();
        let mut seed = |kind: &str, id: &str, ms: i64, set: BTreeMap<String, Value>| {
            ops.push(RowOp {
                kind: kind.to_string(),
                id: id.to_string(),
                op: OpKind::Upsert,
                set: Some(set),
                hlc: encode_hlc(ms.max(1), 0, "migration"),
                clocks: None,
            });
        };
        for device in &state.devices {
            let ms = newest(&[device.last_seen_at, device.created_at]);
            seed(
                KIND_DEVICES,
                &device.id,
                ms,
                fields([
                    ("id", json!(device.id)),
                    ("name", json!(device.name)),
                    ("platform", json!(device.platform)),
                    ("lastSeenAt", opt_ms(device.last_seen_at)),
                    ("createdAt", opt_ms(device.created_at)),
                    ("version", opt_str(device.version.as_deref())),
                    ("capabilities", json!(device.capabilities)),
                ]),
            );
        }
        for space in &state.spaces {
            let ms = newest(&[space.git_checked_at, Some(space.created_at)]);
            seed(
                KIND_SPACES,
                &space.id,
                ms,
                fields([
                    ("id", json!(space.id)),
                    ("deviceId", json!(space.device_id)),
                    ("path", json!(space.path)),
                    ("name", opt_str(space.name.as_deref())),
                    ("gitDetected", json!(space.git_detected)),
                    ("gitCheckedAt", opt_ms(space.git_checked_at)),
                    ("checkoutId", opt_str(space.checkout_id.as_deref())),
                    ("createdAt", json!(space.created_at.timestamp_millis())),
                ]),
            );
        }
        for chat in &state.chats {
            let ms = newest(&[
                chat.last_message_at,
                chat.last_seen_at,
                Some(chat.created_at),
            ]);
            let config = match &chat.config {
                Some(config) => serde_json::to_value(config)?,
                None => Value::Null,
            };
            let mut row = fields([
                ("id", json!(chat.id)),
                ("deviceId", json!(chat.device_id)),
                ("title", opt_str(chat.title.as_deref())),
                ("archived", json!(chat.archived)),
                (
                    "settlementOverride",
                    chat.settlement_override
                        .as_ref()
                        .map(serde_json::to_value)
                        .transpose()?
                        .unwrap_or(Value::Null),
                ),
                (
                    "pinnedOrder",
                    chat.pinned_order
                        .as_deref()
                        .map(|order| json!(order))
                        .unwrap_or(Value::Null),
                ),
                ("cwd", opt_str(chat.cwd.as_deref())),
                ("branch", opt_str(chat.branch.as_deref())),
                ("checkoutId", opt_str(chat.checkout_id.as_deref())),
                ("config", config),
                (
                    "lastMessagePreview",
                    opt_str(chat.last_message_preview.as_deref()),
                ),
                ("lastMessageAt", opt_ms(chat.last_message_at)),
                ("createdAt", json!(chat.created_at.timestamp_millis())),
                (
                    "harnessSessionId",
                    opt_str(chat.harness_session_id.as_deref()),
                ),
                (
                    "harnessSessionCwd",
                    opt_str(chat.harness_session_cwd.as_deref()),
                ),
                ("spaceId", opt_str(chat.space_id.as_deref())),
                ("lastSeenAt", opt_ms(chat.last_seen_at)),
                ("manuallyUnread", json!(chat.manually_unread)),
            ]);
            // Seeding must not drop persistent child lineage/outbox.
            if let Some(child) = &chat.child_thread {
                row.insert("childThread".into(), serde_json::to_value(child)?);
            }
            seed(KIND_CHATS, &chat.id, ms, row);
        }
        for session in &state.sessions {
            let ms = newest(&[Some(session.updated_at), session.started_at]);
            seed(
                KIND_SESSIONS,
                &session.chat_id,
                ms,
                fields([
                    ("chatId", json!(session.chat_id)),
                    ("deviceId", json!(session.device_id)),
                    ("status", serde_json::to_value(session.status)?),
                    ("lastCompletedTurn", json!(session.last_completed_turn)),
                    ("startedAt", opt_ms(session.started_at)),
                    ("updatedAt", json!(session.updated_at.timestamp_millis())),
                ]),
            );
        }
        let count = ops.len();
        // Chunk so a huge legacy workspace never exceeds the server's batch cap.
        for chunk in ops.chunks(400) {
            self.enqueue_ops(chunk.to_vec());
        }
        Ok(count)
    }
}

// ── field helpers ───────────────────────────────────────────────────────────

fn fields<const N: usize>(entries: [(&str, Value); N]) -> BTreeMap<String, Value> {
    entries
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

fn opt_str(value: Option<&str>) -> Value {
    match value {
        Some(v) => json!(v),
        None => Value::Null,
    }
}

fn opt_ms(value: Option<DateTime<Utc>>) -> Value {
    match value {
        Some(at) => json!(at.timestamp_millis()),
        None => Value::Null,
    }
}

fn newest(candidates: &[Option<DateTime<Utc>>]) -> i64 {
    candidates
        .iter()
        .flatten()
        .map(|at| at.timestamp_millis())
        .max()
        .unwrap_or(1)
}

fn row_to<T: serde::de::DeserializeOwned>(row: &RegistryRow) -> Option<T> {
    let value = Value::Object(row.fields.clone().into_iter().collect());
    match serde_json::from_value::<T>(value) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            tracing::warn!(kind = %row.kind, row = %row.id, error = %err, "skipping malformed registry row");
            None
        }
    }
}

// SessionStatus needs to serialize to the same strings the loro doc used
// ("idle"/"working"/…) — luna_proto's serde derives already use camelCase;
// the compile-time check lives in the tests below.

#[cfg(test)]
mod tests;
