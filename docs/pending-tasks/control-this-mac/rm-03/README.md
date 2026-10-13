# RM-03: Host task projections and one native client runtime

Source before the change: `6a0ad25c` (IOS-00 included). Live status belongs to
`pending_tasks.py`. This is the contract and the evidence plan; the
orchestrator records results here once it has run them. The failure cases
below were written before the code.

## Baseline (rebased on current code)

- **RM-01/RM-02:** the gateway (`descriptor`, `pair`, `session/check`,
  `socket-ticket`, `ws`) with a per-socket remote guard, 32 requests or
  streams in flight and 1 MiB messages; enrollment and saved Macs on the phone.
- **CORE-05:** `luna-sdk` with lost-reply readback and the task-command watch
  with cursor repair.
- **IOS-00:** `luna_mobile::session::RemoteSession`: one pinned socket through
  the SDK, explicit cancellation, coalesced main-actor reads. It names RM-03 as
  the owner of reconnect, caches and projections, on this same type.

**Gaps found:**
1. No remote task list. The phone could read only `engineInfo` and a task's
   command receipts (IOS-00 watched an unknown task id). The local
   `WatchChats`/`WatchSpaces`/`WatchSessions` are owner-only, untyped and
   carry host paths.
2. No cursor or revision for a list or a transcript: `WatchDocMessages` resends
   the whole transcript on every reset and can exceed the 1 MiB message bound.
3. No reconnect: a lost socket was terminal, and every foreground rebuilt the
   session, losing what the phone had shown.
4. No cache, no freshness and no host key: nothing distinguished live data
   from what an earlier connection or run had seen.
5. The transport could open one socket per lifetime.

## What changed

**Host (`remote/projections`).**
- `luna_proto::remote_projection`: the wire contract (cursor, host, rows,
  registry and detail snapshots, deltas, `current`, `removed`, bounds).
- `WatchRemoteTasks` and `WatchRemoteTask`: catalog rows `Watch`/`Stream`,
  target `local`, authority `remote(tasks-read)`; capability
  `remote-projections-v1` in `EngineInfo`.
- `crates/engine/src/remote_projections.rs`: one owner per engine (shared by
  its local and remote services), computing rows from the existing owners:
  workspace registry rows, local live sessions, the chat document's published
  transcript and the durable task-command store. No second store.

**Shared client runtime (`client/runtime`).**
- `luna_sdk`: the two operations, `Client::watch_remote_tasks`/
  `watch_remote_task` (`ProjectionStream`) and `ProjectionStore`, the
  host-keyed, ordered, bounded, immutable state every client applies items to.
- `luna_mobile::session::RemoteSession::following`: the IOS-00 connection
  owner in runtime mode (reconnect, lifecycle, followers, cache).
  `luna_mobile::follow`: the FFI projection records. `FACADE_VERSION` is 2.

**Native (`ios/transcript`, minimal slice).**
- `PinnedSocketTransport`: `closeSocket`, sequential sockets on one session.
- `RemoteHostSession(following:cache:)`: lifecycle calls and revision-gated
  projection reads. `RemoteRuntimeAdapters.swift`: `NetworkPathObserver`
  (`NWPathMonitor`) and `RemoteTaskCache`.
- `PairedMacsModel`: the runtime replaces IOS-00's one-socket live session;
  it survives closing the sheet and backgrounding.
- `RemoteTasksView.swift`: the Mac's tasks with an explicit freshness line, and
  one task's recent history. Full transcript and shell stay with IOS-01/02.

## Contract

**Two projections, two costs.**
- **Registry** (`WatchRemoteTasks`): this host's listed projects (by display
  name, never a path) and tasks as cheap rows: id, project, title, status,
  archived, pinned, unseen, provider, created, last activity and a preview,
  each bounded to 240 bytes. Host order: pinned first, then the most recent
  activity, then id. At most 500 tasks and 200 projects (`totalTasks` counts
  the rest). Only tasks this device owns, in a project that is not removed.
  The snapshot also carries provider readiness (installed, enabled).
- **Detail** (`WatchRemoteTask`): one task's row, its durable
  `taskRevision`/`taskSequence`, and the newest transcript entries, oldest
  first: at most 150 entries, 2 KiB of text each and 256 KiB in total, with
  tool kinds and counts of parts not shown. A task the host does not list is
  answered `removed` without opening (and so creating) a document.

**Identity, cursors and freshness.**
- Every item names its `ProjectionHost`: device, environment (profile), scope,
  profile id, generation, engine version and `capabilities`
  (`remote-projections-v1`; task control is absent until RM-04).
- A cursor is `(generation, revision)`. The generation is fresh per engine
  start. Revisions come from one counter and move only when a projection's
  computed rows change, computed under the owner's lock from the newest
  values, so a newer revision never carries older data. The host keeps no
  replay log.
- A watch with the client's cursor: equal is `current` (nothing resent),
  anything else (including another generation) is a `snapshot`, the repair.
  Each later `delta` names its `base`; rows added or removed always come with
  the full `order`.
- `observedAtMs` is the host's clock. The client adds its own confirmation
  time and a freshness: `live` only while the connection that confirmed the
  view is current and following it, `stale` after that connection ended or
  the task was unfollowed, `cached` when restored from the phone's cache.
- Bursts are coalesced on the host (100 ms registry, 50 ms detail); under a
  slow reader the transport's bounded queue holds back the stream, and the
  watch channels keep only the newest value.

**Client runtime.**
- `RemoteSession::new` is unchanged (IOS-00: one socket, never reconnects).
  `RemoteSession::following(mac, endpointIndex, transport, listener, cache)` is
  the runtime: `connect` starts it; `suspend`, `resume`, `networkChanged`,
  `followTask`/`unfollowTask` drive it; `remoteTasks`, `remoteTask`,
  `projectionRevision`, `projectionIssue`, `cacheNote` and `cacheBytes` read it.
- **Every socket is fully verified:** descriptor, a fresh ticket bought with
  the saved bearer, the pinned socket and `engineInfo`. A stale socket, a
  cached row or a public descriptor never counts as readiness.
- **Reconnect:** a lost socket retries after 250 ms, then doubling to 30 s,
  ±20% jitter. Resume and a returning network retry at once. `rePair`,
  trust, identity, incompatibility, a denial or unsupported access end the
  runtime (`closed`); offline, timeout and malformed replies retry.
- **Generations:** each verified socket starts a new epoch. The store refuses
  updates from any other epoch, socket and request ids are never reused, and
  a projection naming another host closes the session as `rejected`.
- **Delivery independent of views:** while open, the core follows the
  registry and up to four tasks (`MAX_FOLLOWED_TASKS`; following a fifth
  stops the least recently followed, whose detail stays cached and turns
  stale). Views only read; closing a view or the sheet stops nothing. Each
  follower resumes from its cursor on the next socket.
- **Coalesced, immutable reads:** every applied item publishes a new `Arc`
  view. The listener is called once until the platform reads `state()`; the
  platform re-reads projections only when `projectionRevision` moved.
- **Bounded:** one registry, at most 8 cached task details, a cache of at
  most 4 MiB, 4 followers plus the registry per socket (within the gateway's
  32 slots), the IOS-00 frame (256) and watch (64) queues.

**Platform adapters.**
- `SocketTransport.close_socket(socketId)` closes one socket; `open_socket`
  may follow; `close()` ends the transport. Late events of a superseded socket
  are never reported (and the core ignores them anyway).
- App background: `suspend` (accepted Mac work continues) and save
  `cacheBytes`. Foreground: `resume`, then RM-02's check as before.
  `NWPathMonitor` → `networkChanged(available:)`; an open socket survives a
  path change until its keepalive or the Mac ends it.
- A runtime `closed(rePair)` forgets the Keychain token, marks the Mac
  "Pair again" and deletes the cache, as RM-02 does for enrollment.

**Data.** IOS-00's table is unchanged; one store is added:

| Data | Store | RM-03 |
| --- | --- | --- |
| Task projections | `Application Support/LunaRemote/tasks-<saved Mac id>.json` | New. Core-encoded, keyed by device, environment and scope, refused for any other; file protection until first unlock, excluded from backup, at most 4 MiB. Written on background and close, deleted on Forget and re-pair. Titles and previews, no credential. |

Upgrade keeps every byte; a downgrade ignores the new file. Edge data
(`LunaDocs`, edge Keychain and preferences) is never read or written.

## Failure contract (before code) and owners

Owners:
- **Host and runtime E2E:**
  `remote_access::gateway::mobile_runtime_follows_mac_tasks_across_suspend_network_restart_and_cursor_repair`
  (real engine, disposable profile, real gateway and TLS pin, the mobile
  facade over the Rust stand-in transport; P1–P11).
- **Store table:** `luna-sdk` `tests/projection_store.rs` (C1–C12): the
  refusals a correct host never triggers.
- **Native E2E:** `LunaTests/RemoteTasksRuntimeTests` (P12, through the
  shipped `PairedMacsModel`, transport, cache and lifecycle adapters).

| # | Failure | Owner and proof |
| --- | --- | --- |
| F1 | The phone shows another Mac's or another project's tasks | P1: ids equal the host's own registry rows; the removed project's task and paths are absent. C2: a projection for another device, profile or scope is refused. P8: another host's cache is refused (`cacheNote`). |
| F2 | The list is expensive or unbounded | Registry rows only, bounded 500/200 with `totalTasks`; detail bounded 150 entries, 2 KiB each, 256 KiB. C5 refuses oversize or repeated rows. |
| F3 | A reconnect resends everything | P4, P6, P7: an unchanged cursor is `current`, revision unchanged. |
| F4 | A gap or a duplicate is applied silently | C3, C4: wrong base, a duplicate, a non-advancing delta or a missing order is refused; the follower resubscribes for a snapshot. |
| F5 | A change made while away is lost | P5: the snapshot repair carries it. P10: a new generation after a Mac restart is a snapshot with every identity kept. |
| F6 | A late item from an old socket changes state | C1, C6, C7: other epochs refused; a new socket's delta before its confirmation refused; evicted followers refused (`followed` check). |
| F7 | Stale data looks live | P4, P6, P10, P11: suspended, offline, Mac down and revoked are `stale`; P7 restored is `cached`; P9 an unfollowed detail is `stale`. Native P12.5/P12.7/P12.8. |
| F8 | A cached row, a stale socket or a descriptor counts as readiness | P6: every reconnect bought a fresh ticket (tickets equal sockets); P11: a revoked bearer ends the runtime at its next attempt. |
| F9 | Retry storm or a dead loop | Backoff 250 ms→30 s with jitter; P11 ends instead of retrying; followers retry at most 3 times per socket. |
| F10 | Delivery depends on an open view | The core follows, not the view: P3's deltas arrive with no reader; the sheet's Done keeps the runtime; P9 re-follow resumes from the cursor. |
| F11 | Too many streams | At most four followed tasks (P9 `followedTasks`), one registry follower. |
| F12 | A deleted task lingers as current | P9: its detail turns `removed`, the registry delta removes it. |
| F13 | Restart loses what was shown, or shows it as fresh | P7 and native P12.8: cached first, then confirmed by `current`. |
| F14 | Forget leaves projections behind | Native P12.10: the cache file is removed with the access. |
| F15 | A projection opens or creates a document for an unlisted task | `removed` is answered before `DocHost::open` (P9 missing ids). |
| F16 | Results or callbacks off the main actor | Native: every wait asserts the main thread (`resultsOnMain`). |
| F17 | IOS-00 regresses | `RemoteSession::new` keeps one socket; IOS-00's Rust and native tests stay unchanged except the new transport method and state names. |

## Criteria mapping

- **projections:** the two operations and their contract above; host owner
  `remote_projections.rs`; P1–P3, P5, P9, P10.
- **client:** `ProjectionStore`, `RemoteSession::following`, the transport and
  lifecycle adapters; P4–P11, C1–C12.
- **verification:** the Rust E2E above, the store table, and the native E2E
  (P12) over a real host; the visible slice is the screenshot step below.
  IOS-01/02 separately own the full native transcript and shell for RM-07.
- **handoff:** the orchestrator's runs and evidence. `luna-engine` changed, so
  LUNA Dev needs a refresh.

## Commands (orchestrator)

**1. Generated contracts** (catalog rows and SDK operations were added; the
`operations.json` hunk was written by hand to match):

```sh
scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract
scripts/cargo.sh run --locked -p luna-sdk --features schema --example generate
git diff --stat -- crates/rpc/contract crates/sdk/contract sdk/typescript/src/generated.ts
```

**2. Checks.**

```sh
scripts/cargo.sh check --locked -p luna-proto -p luna-rpc -p luna-sdk -p luna-mobile --all-targets
scripts/cargo.sh check --locked -p luna-mobile --features bindgen --bin uniffi-bindgen
scripts/cargo.sh check --locked -p luna-engine --tests
scripts/cargo.sh clippy --locked -p luna-sdk -p luna-mobile -p luna-engine --all-targets
```

**3. Tests.**

```sh
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-rpc --features schema --test contract
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-rpc remote_auth
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-sdk --features schema --test generated
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-sdk --test projection_store
CARGO_INCREMENTAL=0 LUNA_REMOTE_TEST_IP=<this Mac's Wi-Fi IP> scripts/cargo.sh test --locked \
  -p luna-engine --test remote_access gateway:: -- --test-threads=1
```

Expected: all pass, including IOS-00's `mobile_session_…` (unchanged
behavior) and the new `mobile_runtime_…` (about 20–40 s: it restarts the
engine). Without `LUNA_REMOTE_TEST_IP` on this Mac the gateway tests print
SKIPPED.

**4. Bindings.** The facade changed (FACADE_VERSION 2): run
`scripts/ios/build-mobile-core.sh` as IOS-00 step 2 describes (the Xcode
build phase regenerates `Luna/Generated/luna_core.swift`; commit it; also
regenerate `RemoteProbe/Generated`). The Swift call sites expect
`RemoteSession.following(mac:endpointIndex:transport:listener:cache:)`,
`suspend()`, `resume()`, `networkChanged(available:)`,
`followTask(taskId:)`, `remoteTasks()`, `remoteTask(taskId:)`,
`projectionRevision()`, `projectionIssue()`, `cacheNote()`, `cacheBytes()`,
`SocketTransport.closeSocket(socketId:)` and the states `.reconnecting`,
`.suspended`, `.waitingForNetwork`.

## Native QA (disposable Simulator, real QA host)

The host is the Rust campaign's own disposable engine (never the primary
profile). Pairing is the host's CreateRemotePairing encoded as a
`luna://pair#…` code, redeemed by the app's own enrollment.

1. Build for testing (IOS-00 step 2, `DD=../../target/ios-dd/rm03`) and
   install `Luna.app` on an owned, disposable Simulator `$SIM`.
2. `CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test remote_access --no-run`
3. `python3 scripts/ios/qa-native-session-sim.py --campaign rm03 --simulator $SIM --run-dir target/qa/rm-03/<UTC stamp>/native --host-ip <Wi-Fi IP> &`
4. After it prints "Native fixture ready":
   ```sh
   xcodebuild test-without-building -project apps/ios/Luna.xcodeproj -scheme Luna \
     -destination "platform=iOS Simulator,id=$SIM" -derivedDataPath target/ios-dd/rm03 \
     -parallel-testing-enabled NO -only-testing:LunaTests/RemoteTasksRuntimeTests \
     -resultBundlePath target/qa/rm-03/<UTC stamp>/native/RemoteTasksRuntimeTests.xcresult
   ```
5. Expected: 1 passed, 0 skipped; coordinator exit 0; `summary.json` has
   every `nativeObservations` flag true, two different `generations`, the
   host's `deviceId` and an empty `secretKeysInPreferences`. Publish only
   `summary.json` and hashes.
6. Visible slice (screenshot evidence): with a QA host paired by the same
   link, open Paired Macs → the Mac card shows "Tasks on this Mac" with
   `Live · confirmed …`; tap a task for its history; background and foreground
   the app (`Stale · …` then `Live`); quit the QA host gracefully (`Stale ·
   last confirmed … · Reconnecting…`); relaunch the app offline (`Cached on
   this phone · …`). Mask the QR in any recording.
7. Afterwards revoke the QA grant on the host (the Rust campaign does this for
   its own pairing), quit the QA host gracefully and erase only `$SIM`.

## Limitations and open questions

- **Simulator only here.** Physical Wi-Fi/cellular transitions and real
  suspension are RM-07/IOS-09; the network step calls the adapter's entry
  point that `NWPathMonitor` drives, and a real path event during that step
  could shorten the waiting state.
- **No replay log:** any cursor change since the client's is a snapshot,
  bounded by the registry and detail limits. Long transcripts show their
  newest 150 entries; paging older history is IOS-01's.
- **Status is the host's own live runs.** A synced profile's other devices'
  tasks are not listed (V1 is local-only).
- **Followed tasks are not persisted** across app restarts; reopening a task
  follows it again. Followed and cached task bounds (4 and 8) are untuned.
- **Task control** (`remote-task-control-v1`) is absent until RM-04; the
  phone shows no composer.
