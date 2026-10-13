# ZI-02: bounded workspace registry history

Baseline: main `8758745a`. The shared queue owns status. Specification:
[implementation § ZI-02](../implementation.md#zi-02--bound-workspace-registry-history),
[research § ZI-02](../research.md#zi-02-local-rows-retain-an-acknowledgement-queue-with-no-acknowledger).
Dependency ZI-01 is done (`0db2a401`) and touches none of these files.

## Where the baseline grows

Proved from `crates/doc/src/registry.rs`, `crates/engine/src/workspace_host.rs`
and `crates/engine/src/lib.rs` at the baseline, and from the persisted
`registry1` JSON format:

| Path | Effect |
| --- | --- |
| Every typed write (`write`, `delete_row_ops`, seeds) → `enqueue_ops` | Appends a `PendingBatch`. Only `ack_batch` removes one, and only a `RegistryClient` calls it. |
| Local profile (`WorkspaceScope::Local`) | `assemble_runtime_inner` sets `edge_enabled = false`; no client ever exists, so nothing is ever retired. |
| `to_bytes` (debounced 1 s save, boot, every guarded task command twice) | Serializes the whole queue. With UUID ids one batch costs about 262 B (rename), 402 B (last message), 464 B (session upsert), 506 B (seen pair); a turn is about 2.2 KB, kept forever. |
| `overlay_row` / `overlay_rows` / `read_all` | Every read replays every queued op of the row, and listing scans the whole queue per id. Each mutation's `row_exists`, and the worker's `publish` after each change, cost O(queue), so cumulative cost is O(writes²). |
| Development profile without a token, synced profile while offline | The same queue. It must be kept: those writes are intent a server has not seen. |

The E2E's replica arm measures the baseline growth law on the unchanged replica
path (`registry-growth-e2e.json` → `replicaBaseline`).

## Identity: permanently local vs offline sync

- The profile decides, never connectivity. `EngineProfile::registry_mode()`
  maps `Local` → `RegistryMode::LocalOnly`, and `Synced` / `Development` →
  `RegistryMode::Replica` (an exhaustive match, so a new scope must choose).
  Engine assembly passes it to
  `WorkspaceHost::open_with_registry_write_handle_and_mode`. Every other
  constructor (`open`, `open_with_registry_write_handle`, `RegistryDoc::new`,
  `from_bytes` of a replica snapshot) stays a replica, which never folds.
- `LocalOnly` is fixed for the doc's lifetime and persisted as an additive
  `"mode":"localOnly"` field. Replica snapshots omit it, so their bytes are
  unchanged; an older build reads a local snapshot as rows plus an empty queue.
- A local-only host refuses an `EdgeConfig` at open and refuses every room
  join, so server frames can never land on rows holding unseen local writes.
- The local and account profiles keep separate store roots; nothing here moves
  data between them.

## Failure contract

| # | Credible failure | Contract | Owning proof |
| --- | --- | --- | --- |
| F1 | Repeated status/rename/placement writes grow `pending` and snapshot bytes without bound in a local profile | Local writes fold into rows through `apply_op`; the persisted queue stays empty; bytes follow the row set and values | Growth E2E: 25,000 and 100,000 writes, queue 0, row count fixed, 75,000 more writes add at most 4 KiB |
| F2 | Absent connectivity treated as local-only: offline intent lost, or a remote tombstone resurrected on reconnect | Only the profile selects `LocalOnly`. A replica keeps every write except an unsent same-row update a later write fully supersedes; updates never revive; deletes win only when causally newer | Reconnect E2E (11 intents queued across a restart, renames of rows A deleted stay deleted, B's delete loses to A's later rename); growth E2E replica arm (≥ 1 op per write) |
| F3 | Coalescing changes an in-flight batch, drops a cascade op, reorders chat labels against session notifications, or hides working → needs-input → working/completed | See the retention rule below: in-flight batches, multi-op batches, `sessions` rows and lifecycle fields are never touched, and nothing moves across another row | Reconnect E2E (in-flight id/content kept; observer feed order; cascade in one frame) and the doc differential test (server rows equal to pushing every write; literal queue shape) |
| F4 | Migration changes HLC ordering, loses a deletion, restores a terminal task to live state, or publishes rows before the guarded snapshot commit | The queue folds in original order with the shared merge, keeping clocks and tombstones; it is retired only by the guarded boot save; a failed save fails the open with the stored bytes unchanged | Migration E2E: failing boot save → no engine, stored SHA-256 equal; then per-field clocks, six tombstones and the terminal session status checked literally |
| F5 | Rollback through `RegistryDoc::from_bytes`, restore/import or restart loses the mode and resumes queueing, or folds a synced replica | The marker travels inside the bytes every reconstruction uses; the host open reconciles bytes with the profile; a replica opening local-only bytes re-queues every row with original clocks | Migration E2E (guarded task-command rollback, then a write still folds; second restart); growth E2E restart; doc test (misuse re-queue converges on a fresh server) |
| F6 | Compaction blocks startup on a huge snapshot, mutates another profile, or creates an edge connection | One O(queue) pass at open, replacing O(queue) work on every read; scoped to the opening profile's store; `LocalOnly` + edge is an open error and joins are refused | Migration E2E large queue (50,000 ops → row-sized bytes, timed); join refusal by review (no production path reaches it) |
| F7 | A rewritten or dropped batch wedges acks | In-flight batches keep id and content; a superseding write is a fresh batch; a dropped batch a dead socket already delivered is superseded on the server too | Reconnect E2E |
| F8 | Server op/byte limits or reconnect cursor semantics change | Coalescing only removes whole single-op batches; chunking, cursor and hello rules are untouched | Code review; existing `registry_client` suite |

## Retention rule for replicas

When a sync-capable replica queues a single-op field write `next`, it examines
only the contiguous run of unsent single-op batches at the tail of the queue
that update `next`'s own row, and drops those whose every field `next`
rewrites with a strictly newer clock. A batch takes part only when it is an
`update` (never creates or revives), carries no per-field clock overrides, is
not a `sessions` row, and writes only latest-value fields: `title`, `name`,
`lastMessagePreview`, `lastMessageAt`, `lastSeenAt`, `manuallyUnread`,
`pinnedOrder`, `branch`, `checkoutId`, `cwd`, `sourceContext`, `config`,
`harnessSessionId`, `harnessSessionCwd`, `gitDetected`, `gitCheckedAt`,
`executionDefaults`. `archived`, `settlementOverride`, `removedAt`, `spaceId`,
`roomGen`, ownership and lineage stay one batch per write.

For any server row state the merged row is unchanged (per field the newer
write wins wherever the older one would; an update never decides existence).
No op crosses another row or a session, lifecycle, deletion or multi-op batch,
so labels stay ordered against status transitions and cascades stay whole.
Opening a replica snapshot replays the same rule over the loaded queue, which
compacts queues written before it and is a no-op on queues it maintained.
Exact reconstruction (`from_bytes`, used by rollback) never compacts. When in
doubt the rule retains: upstream's broader cross-row merging was not adopted.

## Snapshot and migration contract

- Open: `from_bytes_in_mode` reconciles in memory only. Nothing is published
  before the boot save, which holds the boot admission permit; if it fails, the
  open fails and the next boot repeats the same deterministic reconciliation.
- Guarded projections capture before/after bytes that include the marker; a
  storage failure restores the exact pre-command doc, mode included.
- `WorkspaceHost::open` without a write handle (tests, helpers) logs a failed
  boot save and persists on the next debounce, reaching the same state.
- Backup verification and local import read rows plus `pending`; local
  snapshots keep `pending: []`, so both keep working unchanged.

## Criteria mapping

| Criterion | Delivered by |
| --- | --- |
| contract | This document |
| delivery | `crates/doc/src/registry.rs` (mode, fold, retention rule, reconciling open); `crates/engine/src/workspace_host.rs` (mode capability, reconciling open, join refusal); `crates/engine/src/profile.rs` (`registry_mode`); `crates/engine/src/lib.rs` (assembly passes the mode); `docs/registry-sync.md` (one paragraph) |
| verification | `crates/engine/tests/registry_growth_e2e.rs` (three E2Es with retained JSON); two differential tests in `crates/doc/src/registry/tests.rs` |
| handoff | The commands below; evidence hashes recorded by the orchestrator |

## Commands

```sh
scripts/cargo.sh check --locked -p luna-doc -p luna-engine
scripts/cargo.sh test --locked -p luna-doc --lib registry::tests
EVIDENCE="$PWD/target/zi02-evidence"   # task-created; remove after hashing
LUNA_ZI02_EVIDENCE_DIR="$EVIDENCE" LUNA_SOURCE_SHA="$(git rev-parse HEAD)" CARGO_INCREMENTAL=0 \
  scripts/cargo.sh test --locked -p luna-engine --test registry_growth_e2e -- --test-threads=1
shasum -a 256 "$EVIDENCE"/*.json
# Regression owners of the touched paths:
scripts/cargo.sh test --locked -p luna-sync --test registry_client
scripts/cargo.sh test --locked -p luna-engine --lib workspace_host
scripts/cargo.sh test --locked -p luna-engine --test workspace_sync --test local_profiles \
  --test local_import --test backup_restore --test task_commands
```

`--test-threads=1` keeps the retained timings uncontended. Expected: every
test passes; `registry-growth-e2e.json` shows `pendingBatches: 0` at both local
checkpoints with near-equal `snapshotBytes`, and the replica baseline grows by
at least 100 B per write. Then refresh LUNA Dev (`scripts/run-macos-dev.sh
--refresh`) and read back an existing local profile's sidebar after its first
boot folds the queue.

## Limitations and open questions

- A synced replica offline for a long time still grows with session
  transitions and interleaved rows. That is the intended price of preserving
  notifications. A development profile with no token is a replica too, and
  only same-row bursts coalesce there.
- Local tombstones are kept: they grow with distinct deleted ids, not writes.
  No tombstone GC was added.
- Rollback via `from_bytes` resets `in_flight` (pre-existing). A write after a
  rollback may then retire a batch that is still in flight. This is harmless:
  the newer write supersedes it on the server, and its ack retires nothing.
- The E2E measures registry bytes, queue sizes and wall times in a debug test
  process, not engine/app RSS. The native readback and footprint belong to the
  LUNA Dev handoff. No 200 MB or startup claim is made.
- The mock server is the transport oracle. The wire protocol is unchanged, so
  the `--ignored` live-edge test was not needed. The iOS `RegistryCore.swift`
  replica is separate and unchanged.
- The join refusal and the open-time edge rejection for `LocalOnly` have no
  production caller and no test.
- CI routing (`.github/workflows/ui-tests.yml`) does not list the new test
  target yet. Adding it is the orchestrator's call.
