# CORE-07: supervised extension execution and capability broker

Baseline: `8758745a` (13 October 2026). Live status belongs to
`pending_tasks.py`. This file is the contract, the failure cases (written
before the code), the criteria mapping and the evidence plan.

## Rebased against current main

What already existed and is reused:
- **CORE-00 ADR** (`core-00/decision.md`): the subprocess worker protocol
  (JSON lines, major/minor, correlated ids, 64 KiB frames bounded before
  decoding, 1 s handshake), Wasmtime 49.0.2 core modules with explicit LUNA
  imports and no WASI, fuel with 10,000-unit async yields, a 16 MiB guest
  memory, and the trust table: native workers are explicitly trusted
  user-level code and are never described as sandboxed. The probes in
  `tools/core-baseline` and `scripts/qa-core-worker.mjs` stay feasibility
  instruments; nothing here links them.
- **CORE-06** (`core-06/README.md`): the package store, manifests, the
  capability vocabulary and the per-profile grants file. Its hand-off asked
  CORE-07 to re-verify the hash and refuse unless the profile grant names
  that exact hash before every start.
- **CORE-01/03**: catalog rows (`localOnly`), command admission and the
  device-write permit for the new runtime-state file.
- `WorkspaceHost::chat`, which already resolves a task to its owning device
  and checkout (as `ProjectCommands` does).

What was missing: any activation, worker process, broker, resource bound,
quarantine or runtime readback. No production code spawned package code.

## Contract

### Runtime and trust (pinned from CORE-00)

| Class | How it runs | What LUNA enforces | What it does not |
| --- | --- | --- | --- |
| `data` | Never runs; starting one is refused | Nothing to run | n/a |
| `wasm` (trust `portable`) | LUNA's own host process, `luna-extension-host` (Wasmtime 49.0.2, core modules, portable profile 1.0), one per instance | Only `luna_broker_v1.call` and `luna_broker_v1.log` are linked; a 16 MiB store limit; fuel per call with async yields; a fresh store per call; the caller's deadline; every process bound below | Nothing beyond the broker is reachable from the guest; the host process itself is not OS-sandboxed |
| `native` (trust `native`) | The package's own entry point, as a child process | The broker's capabilities and target, protocol bounds, its own session and process group, an allow-listed environment, no inherited descriptors, sampled group memory and process count, `RLIMIT_CPU`, `RLIMIT_NOFILE`, `RLIMIT_CORE` 0 | **No OS sandbox.** Its filesystem, network and process access are the user's. It can dial the loopback owner IPC port itself, read other files of the account, and let a child start its own session, which leaves the sampled group |

Every instance view carries `isolation: {osSandbox: false, enforced, notEnforced}`.
Native views list `filesystem`, `network`, `processSpawn`, `loopbackOwnerIpc`,
`otherUserProcesses`, `diskUsage` and `childSessions` as not enforced. The
`process.spawn` and `network` grants are recorded and shown, but a native
worker has those powers anyway, and the portable profile has no import for
them, so a guest cannot use them at all.

The portable host is found next to the LUNA executable
(`luna-extension-host`), or at the absolute path in `LUNA_EXTENSION_HOST`
(the developer override for builds that do not bundle it). Without it a Wasm
package is refused with an explicit message. Wasmtime stays out of the app
and the engine: only the host binary links it.

### Handshake (worker protocol 1.0)

1. The engine writes `hello` first: `{type, id, protocol: {1,0}, api: {1,1},
   instance: {id, extensionId, version, packageHash, trust, grants, target},
   limits, required, portable?}`. A native task target also carries the
   checkout `root`, which a native worker can find anyway.
2. The worker must answer within 1 s (3 s for the portable host, which
   compiles first) with `{type: "hello", id: <same>, protocol: {1, m},
   extensionId, packageHash, operations, portable?}`.
3. Refused, and the group killed: another major; a different id; another
   extension id or package hash; a required operation (`require` on start)
   not advertised by name; invalid JSON; a frame over 64 KiB; end of output;
   silence. The negotiated minor is the smaller one. Unknown hello fields are
   ignored and grant nothing; trust comes from the manifest, never the worker.
4. Portable only: `portable.moduleSha256` must equal the manifest's pin for
   `runtime.module`. The host hashes exactly the bytes it compiles.

After the hello: engine → worker `call {id, method, params, deadlineMs}`,
`cancel {id}`, `hostResult {id, ok, result|error}`, `shutdown`; worker →
engine `response {id, ok, result|error}`, `hostCall {id, op, params}`,
`hostCancel {id}`, `log {level?, message}`. `hostCall` and `hostCancel`
refuse unknown fields (answered `badRequest`, not fatal). Fatal protocol
violations: oversized or invalid frames, unknown frame types, a response to
a call that is not pending. Workers exit on end of input.

Call statuses are the same for every worker class: `ok`, `failed` (the
worker's error), `deadlineExceeded`, `workerExited` and `busy`. The
supervisor's timer owns the deadline and sends `cancel`; a worker that
stops a call at its `deadlineMs` itself and answers with error code
`deadlineExceeded` is reported as `deadlineExceeded` too. The portable host
only does so 250 ms after the deadline, as a backstop when no cancel arrives.

### Principal and target (server-owned)

`StartExtensionInstance {extensionId, packageHash, taskId?, require?}` does,
before any process exists: the exact hash is installed under that id and
enabled in this profile; the stored grant names only requested capabilities
and every required one; every file re-hashes to the manifest; the hash is
not quarantined; a native package holds the `native-code` grant; the task,
when given, exists on this device with a local checkout (canonicalized).
The server then binds the principal (instance id, extension id, version,
hash, sorted grants) and the target. Every restart repeats all of it and
refuses if the grants or target changed.

### Broker operations (extension API 1.1)

| Operation | Capability | Parameters (unknown fields refused) |
| --- | --- | --- |
| `host.info` | none | none; returns the principal, target and operation list |
| `workspace.read` | `workspace.read` | `path`, `offset?`, `maxBytes?` (at most 32 KiB per call, base64) |
| `workspace.write` | `workspace.write` | `path`, `data` (base64, at most 32 KiB), `create?`; temp file, fsync, rename; keeps the mode |
| `tasks.list` | `tasks.read` | none; at most 64 local tasks (id, title, project, archived) |

Anything else, including every owner RPC method, is `unknownOperation`.
Workspace paths are relative to the bound task's checkout, canonical (no
`.`, `..`, empty segments, `\` or NUL), at most 1,024 bytes. Each folder is
opened with `openat(O_NOFOLLOW | O_DIRECTORY)` after an `lstat`, files with
`O_NOFOLLOW | O_NONBLOCK`, so links, including ones swapped in mid-call, are
refused (`outsideTarget`). Before each workspace call the task must still
exist on this device with the same checkout (`targetChanged`). Error codes:
`denied`, `outsideTarget`, `badRequest`, `unknownOperation`, `noTarget`,
`targetChanged`, `revoked`, `notFound`, `tooLarge`, `busy`, `timeout`,
`failed`. A host call cancelled after dispatch may still commit; a write is
atomic, so it either happened entirely or not at all.

### Bounds

| Bound | Value |
| --- | --- |
| Control frame | 64 KiB each way, checked before decoding |
| Frames | burst 4,096, then 2,048/s; more stops the worker (`flood`) |
| Calls per instance | 8 in flight; more is `busy`, nothing sent |
| Call deadline | `timeoutMs`, default 30 s, at most 120 s; then `cancel` |
| Cancel grace | 1 s for an answer to a cancelled call, or the group is killed (`hung`) |
| Host calls | 16 in flight, burst 100 then 50/s (`busy`), 5 s each (`timeout`) |
| Input stall | a worker that does not read its input for 2 s while frames wait is stopped (`unresponsive`) |
| Log window | newest 256 lines and 64 KiB (worker `log` frames and stderr lines, 2 KiB each); stderr is always drained and counted |
| Memory | group resident memory sampled every 200 ms: native 1 GiB, portable host 256 MiB, or the manifest's `runtime.limits.memoryMiB` (16–4,096) |
| CPU | `RLIMIT_CPU` per process: 3,600 s or `runtime.limits.cpuSeconds` (1–86,400); hard limit 2 s higher |
| Processes | 64 per native group, 4 for the portable host (sampled) |
| Open files | `RLIMIT_NOFILE` 1,024 native, 64 portable |
| Instances | 16 running per engine; 32 records kept for readback |
| Guest (portable) | 16 MiB linear memory, 1 instance/memory/table, 10,000 table elements, 2×10⁹ fuel per call, 512 KiB Wasm stack, 8 MiB module, 256 `log` calls per call |

`runtime.limits` is the API 1.1 manifest addition; a package using it must
set `api.minMinor` to 1.

### Lifecycle, faults and quarantine

- Nothing starts when the engine opens. Instances start only on request.
- A call goes to the running worker. A crashed worker restarts only when the
  next call asks, after full re-verification (new generation, new pid).
- A call in flight when its worker exits fails `workerExited`, with the
  exit. Calls are never retried or replayed.
- Faults: unexpected exits, failed handshakes, protocol violations, `hung`,
  `unresponsive`, `flood` and limit kills. Three within ten minutes for one
  package hash quarantine it in `{profile store root}/extension-runtime.json`
  (temp file, fsync, rename; profile-bound like the grants file). Every
  instance of that hash stops; starts and restarts are refused, across
  engine restarts, until `ReleaseExtensionQuarantine`.
- Disabling a package, or enabling it with a different grant set, revokes
  running instances: the principal is marked first (later broker calls are
  `revoked`), then the worker stops.
- Stop and shutdown: `shutdown` frame and end of input, then `SIGTERM`, then
  `SIGKILL`, 500 ms apart, for the whole group. Engine shutdown stops every
  worker; dropping the engine without shutdown kills them.
- The supervisor watches the group leader with `waitid(WNOWAIT)`, kills
  what remains of the group while the leader is unreaped (so the group id
  cannot have been reused), then reaps and records `groupCleared`.

### Owner methods

All five are catalog rows with `local` target and `localOnly` authority,
served only by the engine's local service, never by an agent tool or a
worker:

| Method | Kind |
| --- | --- |
| `ListExtensionInstances` | query |
| `StartExtensionInstance {extensionId, packageHash, taskId?, require?}` | command |
| `CallExtensionInstance {instanceId, method, params?, timeoutMs?}` | command |
| `StopExtensionInstance {instanceId}` | command |
| `ReleaseExtensionQuarantine {extensionId, packageHash}` | command |

Types: `crates/proto/src/extension_runtime.rs`.

## Failure contract (written before the code)

Each case maps to its owner test below. Native cases live in
`crates/engine/tests/extension_runtime.rs`; portable (Wasm) cases in
`crates/engine/tests/extension_portable.rs`.

| ID | Failure | Defence |
| --- | --- | --- |
| R1 | A worker speaks another protocol major, sends invalid JSON, an oversized or wrong-id hello, exits or stays silent, and is still activated | Hello within the deadline, bounded before decoding, exact correlation id, major 1 only; every refusal kills and reaps the process group |
| R2 | A worker answers for a different package (id or hash) | The hello must echo the server's extension id and package hash |
| R3 | A newer minor's optional additions satisfy an operation the caller requires | Required operations must be advertised by name; unknown hello fields grant nothing |
| R4 | The portable host compiles different bytes than the manifest pins | The host hashes the exact bytes it compiles; the hello must report the manifest's module SHA-256 |
| R5 | A package that is not enabled, enabled at another hash, data-only, tampered after enable, quarantined, or native without the native-code grant starts a process | Every start and restart re-reads the profile grant for the exact hash, re-hashes every file and checks quarantine before spawning |
| R6 | A worker names its own principal or target (`instanceId`, `taskId`, `grants`, `target`) | Broker requests refuse unknown fields; the principal and target are bound by the server at start |
| R7 | `..`, an absolute path, a symlink (or a swap to one) reaches another task's files | Paths are relative and canonical; every component is opened with `openat` and `O_NOFOLLOW`; links are refused |
| R8 | An ungranted capability is used | Each broker operation names its capability; ungranted calls are `denied` and change nothing |
| R9 | A worker reaches owner IPC or an owner RPC method | The broker has no owner methods; environment is an allow-list; inherited descriptors are closed; a new session detaches the terminal |
| R10 | A worker keeps its authority after disable or a grant change | Revocation marks the principal before stopping; later broker calls are `revoked` |
| R11 | The target task disappears or its checkout changes, and the old root is still used | Each workspace call re-checks the task's device and canonical checkout |
| R12 | A call is never answered and the caller waits forever, or a blocked worker keeps running | Caller-side deadline; a cancel frame; no acknowledgement within the grace period kills the group |
| R13 | Frame, log, stderr or host-call floods starve the engine or grow memory | Bounded frames, a frame-rate cap that stops the worker, a bounded log ring, drained stderr, host-call rate/concurrency caps with `busy` replies |
| R14 | A worker's process group grows past its memory budget | Group resident memory is sampled; over budget stops the group (`memoryLimit`) |
| R15 | A worker spins forever | `RLIMIT_CPU` per process (`cpuLimit`) |
| R16 | A worker forks without bound | Group process count is sampled; over the cap stops the group (`processLimit`) |
| R17 | Concurrent calls or instances grow without bound | Per-instance call concurrency (`busy`) and an engine-wide running-instance cap |
| R18 | A Wasm guest loops, grows memory, imports WASI, traps, or is cancelled while its host calls keep running | Fuel with async yields and the call deadline; a 16 MiB store limit; a linker with only `luna_broker_v1`; a fresh store per call; pending host calls are cancelled when the call is |
| R19 | A crash mid-call takes the engine or other work down, or the call is replayed | The call fails `workerExited` and is never retried; other instances and tasks are untouched; the next call restarts after re-verification |
| R20 | A crash-looping package keeps restarting | Three faults in ten minutes quarantine the exact package hash, durably, until the owner releases it |
| R21 | A worker survives engine shutdown | Shutdown stops every group (shutdown frame, `SIGTERM`, `SIGKILL`) and reaps it |
| R22 | An engine restart re-runs instances or replays calls | Nothing starts until asked; no call is persisted for replay |
| R23 | A native worker is described as sandboxed | The instance view states `osSandbox: false` and lists what is not enforced |

| Owner test | Cases |
| --- | --- |
| `extension_runtime::handshake_and_preflight_refusals_leave_nothing_running` | R1–R3, R5, R23 (8 handshake packages, 3 preflight refusals) |
| `extension_runtime::broker_confines_workers_to_their_grants_and_bound_task` | R6–R10, R21, R23 |
| `extension_runtime::hangs_floods_and_resource_limits_stop_only_the_faulty_worker` | R12–R17, R19 (unrelated instance, task files and task row) |
| `extension_runtime::crashes_are_contained_never_replayed_and_quarantined_across_restart` | R5 (tamper after enable), R19–R22 |
| `extension_portable::portable_guests_reach_only_the_broker_within_their_bounds` | R4, R6–R8, R13, R18, R19 |

Every test drives the public RPC methods against a real engine, real
package bytes in temp directories and real child processes. Independent
readback: `kill(pid, 0)` and `kill(-pgid, 0)` from the test process, SHA-256
of every file in each task's checkout, the grants and runtime-state files,
and the task registry row. The native peer is an independently written
Python worker; the portable guests are WAT assembled by the test with the
`wat` crate. The package hash and the module hash are recomputed from the
published rules, never read back from the engine.

These risks are **not tested**:
- R11 (`targetChanged`): the defence is in the broker, but no test moves a
  task's checkout under a running instance.
- The `unresponsive` stop (a worker that never reads its input).
- Wasm module-hash mismatch: the hash comes from the host, so a test would
  need a different host binary. R4 is covered by the positive readback only.
- A swap to a symlink during a call (the `openat` race the design closes).
- Linux sampling (`/proc`) and every non-macOS platform: the suites are
  `#![cfg(unix)]` and were written against macOS.

## Criteria mapping

- **contract:** this file and `crates/proto/src/extension_runtime.rs`: the
  CORE-00 runtime and trust table, the handshake, version and hash rules,
  the server-owned instance/target/grant principal, the broker vocabulary,
  the bounds and the owner methods.
- **delivery:** `crates/engine/src/extensions/runtime.rs` and
  `runtime/{protocol,worker,process,broker,quarantine}.rs` (supervisor,
  frames, process control, broker, quarantine), the manager hooks in
  `crates/engine/src/extensions.rs`, the RPC handlers in
  `crates/engine/src/rpc/extensions.rs`, and the portable host
  `crates/extension-host`.
- **verification:** E09 is the five tests above, with OS process/group and
  file readback and the evidence below. Native views never claim a sandbox.
- **handoff:** the orchestrator's build, E2E, lint, review, LUNA Dev refresh
  and delivery.

## Commands (orchestrator)

The portable host adds Wasmtime 49.0.2 to the workspace lockfile (the
`crates/extension-host` member and the engine's `wat` dev-dependency), so
the first build must update `Cargo.lock` without `--locked`; later builds
use `--locked` as usual.

```sh
python3 scripts/build-budget.py --status
scripts/cargo.sh check -p luna-extension-host                 # first build: updates Cargo.lock
scripts/cargo.sh check --locked -p luna-proto -p luna-rpc -p luna-engine -p luna-extension-host
scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract   # regenerate operations.json (+5 rows; already added by hand)
scripts/cargo.sh test --locked -p luna-rpc --features schema --test contract
scripts/cargo.sh test --locked -p luna-rpc --lib remote_auth::tests::policy_inventory_is_complete_and_unknown_fails_closed
scripts/cargo.sh build --locked -p luna-extension-host
EVIDENCE="$PWD/target/qa/core07/$(date -u +%Y%m%dT%H%M%SZ)"
LUNA_CORE07_EVIDENCE_DIR="$EVIDENCE" CARGO_INCREMENTAL=0 \
  scripts/cargo.sh test --locked -p luna-engine --test extension_runtime -- --nocapture
LUNA_EXTENSION_HOST="$PWD/target/debug/luna-extension-host" LUNA_CORE07_EVIDENCE_DIR="$EVIDENCE" CARGO_INCREMENTAL=0 \
  scripts/cargo.sh test --locked -p luna-engine --test extension_portable -- --nocapture
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test extension_packages
scripts/cargo.sh clippy --locked -p luna-proto -p luna-rpc -p luna-engine -p luna-extension-host --all-targets
scripts/cargo.sh check --locked -p luna-ui
```

The native suite needs `python3` on `PATH` (the peer's shebang is
`/usr/bin/env python3`, as the harness peers use). Retain `$EVIDENCE` with a
SHA-256 manifest.

## Native QA (LUNA Dev, local data only)

LUNA Dev has no instance UI yet; drive its owner IPC (port 59777) with
Node's built-in WebSocket. Install and enable the native QA package from
`core-06/README.md` (`/tmp/core06-qa/echo`) first. Its `bin/worker` does not
speak the protocol, so a start is a refused handshake:

```sh
node - <<'JS'
const ws = new WebSocket('ws://127.0.0.1:59777');
let id = 0; const pending = new Map();
const call = (method, params = {}) => new Promise(done => { pending.set(++id, done); ws.send(JSON.stringify({id, method, params}) + '\n'); });
ws.onmessage = ({data}) => { for (const line of String(data).split('\n').filter(Boolean)) { const f = JSON.parse(line); pending.get(f.id)?.(f); } };
ws.onopen = async () => {
  const list = await call('ListExtensions');
  const echo = list.ok.packages.find(p => p.id === 'core06.qa/echo');
  console.log(JSON.stringify(await call('StartExtensionInstance', {extensionId: echo.id, packageHash: echo.packageHash}), null, 2));
  console.log(JSON.stringify((await call('ListExtensionInstances')).ok, null, 2));
  ws.close();
};
JS
```

1. Expected: `err` contains `failed its handshake`; the listing shows the
   instance `failed`, `lastExit.reason: handshakeFailed`,
   `groupCleared: true`; `ps -p <lastExit.pid>` finds nothing.
2. Repeat twice more: the third fault quarantines the hash; the listing's
   `quarantined` names it and a fourth start is refused as quarantined.
3. Quit LUNA Dev gracefully and relaunch: the listing has no instances and
   the quarantine is still there.
4. `ReleaseExtensionQuarantine` with the same id and hash empties it.
5. Settings → Extensions still lists and disables the package normally.

A protocol-speaking worker for manual runs is `FIXTURE_WORKER` in
`crates/engine/tests/support/extension_runtime_fixture.rs` with a
`fixture.json` of `{}`. LUNA Dev's `PATH` is the launchd default, so
`/usr/bin/env python3` resolves to `/usr/bin/python3`. The bundle does not
include `luna-extension-host` yet, so Wasm packages are refused there with
the explicit message.

## Limitations and hand-offs

- **Bundling:** `scripts/run-macos-dev.sh` and the release packaging do not
  copy `luna-extension-host` next to `luna` yet. Until they do, the app
  refuses Wasm packages honestly. Owner: the orchestrator/packaging.
- **Native trust:** no OS sandbox. A native worker can reach the loopback
  owner IPC port, the user's files and the network; only what LUNA brokers is
  scoped. A child that calls `setsid` leaves the sampled group, and its memory
  and processes are no longer counted. Disk usage is not bounded.
- **Engine crash:** workers get end of input when the engine dies. A
  protocol-1.0 worker exits; a worker that ignores it outlives the engine,
  and the next engine does not adopt or reap it.
- **CPU:** `RLIMIT_CPU` is per process and cumulative. A worker that spins
  between calls is bounded only by that limit.
- **Memory:** sampled every 200 ms, so a short burst can overshoot before
  the stop. Linux reads `/proc`; other Unix platforms enforce neither memory
  nor process caps, and their views omit `residentMemory`/`processCount`
  from `enforced`. Windows refuses to start workers.
- **Host calls:** a cancelled host call may still commit; writes are
  atomic. Portable calls are stateless (fresh store per call).
- **CORE-08:** provider registration, operation schemas and the provider
  profile build on `StartExtensionInstance` (with `require`) and
  `CallExtensionInstance`. **CORE-18:** in-app authoring and confirmation.
  **CORE-19:** updates must drain or stop instances pinned to the old hash;
  removal must release that hash's quarantine record. **CORE-20:** measured
  idle and per-instance cost.
