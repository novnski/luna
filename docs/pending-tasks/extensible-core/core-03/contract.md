# CORE-03 contract: engine services and lifetimes

Source: `d81a9d4d` (12 October 2026). This records the engine as it is
composed today and the rules each extraction must keep. Milestone 1 (the
lifetime and admission owner) is delivered against it. The remaining owners are
specified here before any code moves.

## 1. Composition today

`EngineCore::assemble_with_profile_locked_and_accounts` (`crates/engine/src/lib.rs`)
is the only composition root. The five public `assemble_*` entry points differ
only in where the InstanceLock comes from and which provider-account or Codex
metadata seam is used. In order, assembly:

1. Refuses a pending managed restore, then takes the device root's
   `InstanceLock`. The headed app takes the lock earlier, before binding IPC.
2. Builds `DeviceWriteOwner` (the write epoch plus the lock) and the device ID.
3. Opens the remote authority store and loads harness enablement (guarded).
4. Opens `DocsStore` (SQLite), the run journal (guarded), `SessionsEngine`,
   `DocHost` and `WorkspaceHost` (guarded).
5. Opens task identity (guarded), then cross-wires them:
   - doc host to workspace and to sessions;
   - sessions to doc host and to task identity;
   - review lookup.
6. Opens side chats (guarded). Runs `recover_stale` and starts transcript
   salvage.
7. Opens repos, change requests, workspace files, terminals, project commands,
   previews and uploads, plus the local importer for synced profiles. Then
   agent accounts, titles, diff sync, checkpoints, GitHub PRs and spaces sync.
8. Starts task-identity reconciliation. Opens backup control, the model and
   usage caches, chat workbenches and provider defaults.

**Per-transport services.** `rpc_service*()` builds a new `EngineRpc` on every
call. A headed or headless runtime holds several at once: the in-process UI
and IPC service, the remote listener's shared inner service, and the host
relay's.
- Each copy has its own composer-catalog cache, child-worktree reservations,
  automation reconcile cursor and activity cancel token.
- `EngineRpc::new` itself starts the activity bootstrap, the model-cache
  refresh and the usage cache.
- The first copy built inside a Tokio runtime starts the five maintenance
  workers: cleanup, child threads, automations, Board review and Board
  lifecycle. The engine keeps that copy for their lifetime.
- Every copy registers as a cross-task backend in a weak "newest live" list,
  and in the Board authoring registry: a process-global table keyed by the
  engine's `BoardHub`.

This is the coupling CORE-03 removes: composition and long-lived workers are
started from an operation-service factory.

## 2. Write and instance ownership

Every writer must keep this order: `InstanceLock` (lifetime), then
`WritePermit`, then the component mutex, file lock or SQLite transaction.

| Writer | Guard today |
| --- | --- |
| Run journal, workspace host, harness prefs, task identity, side chats, repos, project commands, uploads, local import, GitHub PR attempts, model cache, chat workbenches, provider defaults | Device write epoch (`DeviceWriteHandle::try_admit`/`admit_async`) |
| `DocsStore` (receipts, Board, automations, drafts, the queue) | **None in production.** `DocsStore::open_guarded` exists only as a seam: mutators on a guarded store fail closed until they take admitted APIs. |
| Terminal history, `previews.json`, `luna-instructions.json`, `ui-settings.json` | None |
| Every RPC `Command` (milestone 1) | **`CommandAdmission`**: one gate per engine, shared by all its `EngineRpc` copies |

**Rules:**
- One engine owns one device root. A second engine on the same root fails at
  `InstanceLock`.
- Only the engine that admitted a command may write for it. A command is
  admitted only while its engine is open. While running, it holds the device
  lease, an `Arc` on the lock owner, so the root cannot be reopened under it.
- Admission closes at the start of shutdown, or on drop, and never reopens. The
  write epoch retires at the end of shutdown.
- **Deferred:** an unguarded writer (`DocsStore`) remains reachable by internal
  engine tasks that keep an `Arc<DocsStore>`. A later milestone moves those
  writers behind admitted APIs, starting with the receipt ledger. Until then,
  closing admission and draining before teardown is the guarantee.

## 3. Service ports

| Owner | Port (what callers depend on) | Status |
| --- | --- | --- |
| `EngineLifetime` | `admission()`, `is_open()`, `close()`, `close_and_drain(timeout)` | **Delivered (M1)** |
| `CommandAdmission` | `admit() -> AdmittedCommand`, held for the handler and any streamed reply | **Delivered (M1)**, applied in `EngineRpc`'s `RpcService::handle` |
| `EngineServices` (composition root) | One builder that owns the assembly order above and returns typed handles. It replaces the five `assemble_*` variants with one hermetic option set. | M2 |
| `MaintenanceSupervisor` | `start(services)` once per engine, `stop()` cancels and joins | M2: moves the five workers, the activity bootstrap and cache starts out of `rpc_service`/`EngineRpc::new`. It also makes the cross-task backend and Board authoring registration engine-scoped instead of per-copy. |
| `RunCoordinator` | Run admission, live-run table, interrupt/settle, provider subprocess ownership (extracted from `SessionsEngine`) | M3 |
| `ProjectionHost` | Queue projection install, command drain, replay cursor and transcript import (extracted from `DocHost`) | M3 |

Each port is an in-process Rust handle; no network hop is added. A domain
service must be constructible headless from `EngineServices` without the UI.

## 4. Start and stop order

**Start:** the assembly order in section 1. Maintenance starts once, after
assembly. Today that is lazily in `rpc_service`, gated by `is_open()`; from M2
it moves to `MaintenanceSupervisor::start`.

**Stop** (`EngineCore::shutdown`, in this order):

1. **Close admission (new).** Then wait up to `COMMAND_DRAIN` (5 s) for
   admitted commands, logging any still running.
2. Stop the usage and model caches, cancel and join cleanup maintenance, and
   shut previews.
3. Pause every queue, so settling a run never releases queued work.
4. Shut down side chats and sessions: interrupt runs, stamp `aborted`.
5. Stop Board discussions, terminals (killing PTYs), agent accounts and change
   requests.
6. Abort and join the updater wake, then shut down the updater, diff sync,
   workspace files and spaces sync.
7. Flush and join doc-host workers, then the workspace snapshot and task
   drain.
8. Break the sessions/doc-host cycle and retire the write epoch.

`EngineRuntime::shutdown` first revokes the host relay and Edge transports.
`Drop` closes admission, stops the caches, cancels and aborts maintenance, and
retires the epoch, without waiting.

## 5. Cancellation

| Resource | Cancelled by |
| --- | --- |
| New RPC commands | `EngineLifetime::close` (M1) |
| Maintenance workers | `cleanup_cancel`, then join (cleanup) or cancel-only (the other four). M2 joins all five. |
| Per-copy activity bootstrap | That copy's `activity_cancel`, on drop |
| Provider runs and subprocesses | `SessionsEngine::shutdown` interrupt and settle |
| PTYs | `Terminals::shutdown` |
| Edge transports and relays | `disconnect_edge` before draining |
| Watch streams | **Not cancelled on close.** They keep serving the stopped engine's last state until the client drops them. Closing them needs a UI retry/backoff review first (deferred, see failures). |

A dropped admission future cancels only that admission, never an admitted
command.

## 6. Backpressure

- **RPC memory transport:** 256-frame channels in each direction. A streamed
  reply is read at the client's pace.
- **Task receipts:** a broadcast of 256. A lagging watcher resynchronises from
  a snapshot.
- **Per run:** steering is bounded at 32. Provider events into the engine are
  **unbounded** per run. This is a recorded gap for the run-coordination
  milestone (M3); it needs measured limits first (CORE-20).
- **Shutdown:** the admission drain is bounded at 5 s. Session settling keeps
  its existing bounds.

## 7. Stale-epoch behaviour

| Situation | Behaviour |
| --- | --- |
| Command on a client kept from a stopped engine | Refused: "This LUNA engine has stopped and accepts no new commands." No receipt and no write. |
| Command on a stale client after the same root reopened | Refused, for the same reason. The new engine's ledger has no receipt, so the command can then apply there. |
| Engine reopens while an admitted command is still running | `InstanceLock` contention until the command returns: the device lease keeps the old owner alive. |
| `rpc_service()` on a stopped engine | Returns a service that refuses commands and starts no maintenance |
| Query or watch on a stopped engine | Served from that engine's last in-memory state. Deferred to the resource milestone. |

## 8. Milestones

1. **M1 (this change):** lifetime and admission owner; L1/L2 keeper in
   `task_commands`.
2. **M2:** composition root and maintenance supervisor; engine-scoped
   registrations; remove side effects from `EngineRpc::new`.
3. **M3:** run coordinator and projection host extracted behind the ports
   above, with E05 restart, identity and race keepers re-run before old paths
   are deleted.
4. **Close-out:** watch cancellation on close, the bounded provider event
   queue, and `DocsStore` writers behind admitted APIs.
