# RM-04: Remote task commands, admission and outbox reconciliation

Source before the change: `11b97743` (RM-03 `742eb2bd`, AW-10, SW-13
included). Live status belongs to `pending_tasks.py`. This is the contract and
the evidence plan; the orchestrator records results here once it has run them.
The failure cases below were written before the code.

## Baseline (rebased on current code)

- **CORE-01/05:** `ExecuteTaskCommand` with durable receipts (`begin_task_command`
  binds `commandId` to task and payload hash), fences (`expectedTaskRevision`,
  `expectedBindingGeneration`, `expectedTurnId`), the session-doc executor
  that only runs rows with an exact local executable grant, and the SDK's
  lost-reply readback by id. The catalog row is `localOnly`.
- **RM-01/RM-03:** the per-socket guard (`RemoteRpcService`) checks a
  method's catalog scope and `targetDeviceId`; scopes are `environment-read`,
  `tasks-read`, `files-read`, `terminal-operate`. RM-03 projects the host's
  listed tasks (`remote-projections-v1`) and runs the phone runtime
  (`RemoteSession::following`).
- **AW-10/SW-13/worktrees:** a Run takes a host turn slot; a Run with a
  `WorktreeSpec` materializes the worktree and waits for project setup
  (`ProjectCommands::wait_for_setup`) before its provider starts.

**Gaps found:**
1. A phone could not issue any task command: `ExecuteTaskCommand` is local
   only, and it trusts caller-supplied `actorId`/`clientId`, carries engine
   intents with local paths and configuration, and exposes intents (rename,
   permissions, provider switch, deletion, child chats) a phone must not use.
2. The guard passed no principal to the engine: nothing could bind a command
   id to the authenticated device session.
3. A transport closing mid-call aborted the engine handler (`serve_connection`
   aborts in-flight requests), so a remote command could be cut between its
   pending receipt and its effect.
4. The queue drain did not wait for a pending Run of the same task: a
   follow-up could start a turn while the first message was still
   materializing its worktree and running setup, and after a failed or
   cancelled setup a queued follow-up was dispatched into the unprepared
   checkout.
5. No client outbox: no draft for a task not yet created, no read-back of a
   lost reply across sockets, suspension or restart, no per-task ordering,
   and no visible queued/held/unknown delivery.

## What changed

**Wire contract (`luna_proto::remote_command`).** `RemoteTaskCommand`
(`deny_unknown_fields`): `commandId`, `taskId`, the three fences, `issuedAtMs`
and a closed `RemoteTaskIntent`: `createTask {projectId}`, `send {messageId,
text, followUp: queue|steer, newWorktree?: {base?}}`, `removeQueued`,
`sendQueuedNow`, `steerQueuedNow` (each `{messageId, expectedQueueRevision}`),
`stop {}`, `setArchived {archived}`, `cancelCapacityWait {commandId}`.
`validate()` bounds ids (command ids `[A-Za-z0-9._:-]{1,128}`, others ≤256
printable), text (≤64 KiB, non-empty), requires a UUID for a new task and a
plain ref for a worktree base. Capability `remote-task-control-v1` (engine
info and projection host).

**Authority (`luna_rpc`).** Scope `tasks-operate`. Method
`ExecuteRemoteTaskCommand` (`luna.core/execute-remote-task-command`, command,
unary, local, `remote(tasks-operate)`). For every `tasks-operate` call the
guard builds a typed `RemotePrincipal` from the session it just validated and
calls the new `RpcService::handle_remote(self: Arc<Self>, principal, method,
params)`; the default implementation refuses. `remote_command_identity()` is
the payload identity: SHA-256 of sorted-key canonical JSON of `{v, taskId,
intent}` (`remote-v1:<hex>`), identical whatever a build's `serde_json` map
order. Receipt actor: `remote-session:<session id>`, client `luna-remote`.

**Engine admission (`crates/engine/src/rpc/remote_commands.rs`).** In order,
before any receipt or provider work:
1. CORE-03 command admission; typed parse (unknown fields and intents are
   `bad params`); `validate()`.
2. The grant as it stands now (`validate_session`, `tasks-operate`) and the
   principal's environment equal to this host's.
3. One admission per command id at a time (process lock).
4. An existing receipt for the id: another actor → refused; same task and
   identity → that receipt as recorded (never re-validated or
   re-translated); otherwise a `commandIdConflict` receipt is recorded.
5. Target: a task this host lists (RM-03's rule), or for `createTask` a
   listed project and an unused id.
6. Translation into the engine intent, with the principal as actor and the
   phone's fences unchanged, executed through the same pipeline
   (`execute_task_command_with_identity`, the receipt's `payloadHash` being
   the remote identity) in a detached task holding the admission guard, so a
   closed socket cannot cut it.

| Remote intent | Engine intent | Notes |
| --- | --- | --- |
| `createTask` | `createChat {spaceId, config}` | Folder and provider from the host; permission mode `full`/`auto` only (see Permissions). |
| `send` to an idle task | `session/run` from the task row | As the desktop: starts the next turn, thaws a queue a Stop froze, still waits for setup. |
| `send` to a busy task | `queueMessage {holdForTurnEnd: true}` | Busy = turn in flight, or a pending local Run (preparing turn, capacity, worktree setup). |
| `send` with `newWorktree` | `session/run` with `WorktreeSpec {project path, base or HEAD}` | Only for a task with no binding, run, entry, queue row or command. |
| `removeQueued` / `sendQueuedNow` / `steerQueuedNow` | same, with `expectedQueueRevision` | Revision conflicts are receipts. |
| `stop` | `session/interrupt` | Needs `expectedTurnId`; a Board worker's stop also stops its reviewers, as locally. |
| `setArchived` | `setArchived` | Needs `expectedTaskRevision`. |
| `cancelCapacityWait` | same | |

**Queue gate (`DocHost::drain_queue`, `ProjectCommands::setup_gate`).** A
queued follow-up waits while the task has a pending locally granted Run, and
while the checkout's setup runs (re-checked every 250 ms). After a failed or
cancelled setup it is claimed and rejected exactly as a dispatch preflight
refusal: the row stays queued, `ExecutionUnavailable`, with a rejected
`queue-dispatch:<task>:<message>:<revision>` receipt naming the setup and
"retry setup, then Send now". `wait_for_setup` is the same check in a loop.

**Client runtime.** `luna_sdk::Client::execute_remote`/`remote_receipt`
(validate id, task, actor and identity; read back before one resend;
`UnknownOutcome` when unreadable); `SdkError::may_have_been_admitted`;
`luna_sdk::outbox::Outbox`, the host-and-session-keyed state machine;
`luna_mobile::session::commands`: the runtime's FFI and reconciler
(`FACADE_VERSION` 3). The desktop's Add sheet now grants `tasks-operate`
(label "Operate tasks"); `luna remote pair --scope tasks-operate`; the phone
accepts the scope.

## Contract

**Outbox (`luna_sdk::outbox`).** One per paired host and session (`HostKey` +
receipt actor); a saved outbox for any other is refused, so re-pairing starts
empty. Each entry is one `RemoteTaskCommand`, id minted once.
- Status, computed: `draft` (behind its task's unapplied create), `waiting`
  (behind an unadmitted earlier command of the same task), `held` (an earlier
  command of the task was refused; `blockedBy` names it; discarding it
  releases), `ready`, `sending`, `uncertain` (answer lost: read back by id
  first), `accepted` (pending receipt), `unknown` (pending,
  `providerAcceptanceUnknown`), `applied`, `refused`.
- Only a host receipt that is exactly the entry's (id, task, actor, identity)
  or a refusal the host answered settles it; a pending read never replaces a
  terminal one.
- Fences are read from the task's snapshot just before the first send (a
  live control: when the user acts), per intent (send/remove: binding;
  steer, stop, send-now, steer-now: binding + turn; archive: revision +
  binding), then frozen: a retry is byte-identical.
- **Offline policy per operation:** create, send (queue), remove, archive and
  capacity withdrawal may be composed offline and go out later. Stop, steer,
  send-now and steer-now are live controls: fenced when the user acts, refused
  at once without an open socket, and refused locally (never sent) if their
  connection ends before they go out.
- Queued-message delivery: after a `queueMessage` receipt is applied, its
  dispatch receipt (`queue-dispatch:<task>:<message>:1`) is read: `queued`,
  `dispatching`, `delivered`, `held` (with the host's reason), `unknown`.
- Bounds: 32 entries, 4 MiB saved, 16 settled kept (oldest pruned, never one
  that holds a later entry of its task).

**Runtime (`RemoteSession::following`).** Each verified socket whose host
advertises `remote-task-control-v1` (and whose access includes
`tasks-operate`) runs one reconciler: read back uncertain entries; send ready
ones oldest first; re-read pending receipts and open deliveries (1 s while
any is open, at most 8 reads a pass). A socket's end turns anything in flight
`uncertain`. New calls: `create_task`, `send_message`, `stop_task`,
`remove_queued`, `send_queued_now`, `steer_queued_now`, `set_task_archived`,
`cancel_capacity_wait`, `outbox`, `outbox_revision`, `outbox_issue`,
`outbox_bytes`, `restore_outbox`, `discard_outbox_entry`. The platform saves
`outbox_bytes` whenever `outbox_revision` moved (protected, non-synced, per
saved Mac; deleted on Forget and re-pair) and restores it before connecting.

**Permissions.** No approval path is added. A phone-created task's config is
the host's provider for new tasks, its saved default model (else the cached
catalog's first) and a permission mode that is never legacy Ask, by the
desktop new-task rule (`default_permission_mode`): the create's `full`/`auto`
when the provider offers it; without one, Auto when
`provider_capabilities_for(harness, model)` supports it; otherwise the create
is refused before anything is created or run with a rejected receipt
`permissionModeRequired` (the phone, IOS-03, then offers Full). An explicit
`ask` is malformed (`bad params`). The mode is recorded in the task's config,
so the desktop shows it; Full and Auto never produce an approval card.
Approvals and questions for legacy Ask chats are RM-05.

## Failure contract (before code) and owners

Owners:
- **Host E2E:** `luna-engine` `tests/remote_task_commands.rs` (real engine,
  real remote guard and sessions, recording provider; three tests).
- **Client E2E:** `remote_access::gateway::mobile_outbox_delivers_commands_…`
  (real gateway, TLS pin, the mobile runtime over the Rust platform stand-in).
- **Outbox table:** `luna-sdk` `tests/outbox.rs` (O1–O10).
- **Native:** `LunaTests/RemoteTaskCommandsTests` against a QA LUNA app with
  real providers.

| # | Failure | Owner and proof |
| --- | --- | --- |
| F1 | A read-only grant mutates (RP-12) | `a_paired_session_…`: `lacks required scope`, no receipt. |
| F2 | A local caller or parameter supplies a principal | Local IPC refused; `actorId`/`clientId` in params are `unknown field`; receipts carry `remote-session:<id>`. |
| F3 | A permitted method carries a disallowed variant (RP-12) | rename, setPermission, delete, createChildChat, raw session intents: `bad params`, no receipt. |
| F4 | Wrong target | Unknown/removed project, unknown/removed-project task, non-UUID/existing new task id: refused, no receipt, nothing run. |
| F5 | Revoked grant admits | Revoked session refused before any receipt; the engine re-checks the grant at admission. |
| F6 | A retry executes twice (RP-10) | Same id/payload resent, on a new connection, after the task exists: the recorded receipt; one run. |
| F7 | Same id, changed payload succeeds | `commandIdConflict` receipt; original receipt unchanged. |
| F8 | Another session claims an id | Refused (`belongs to another client`). |
| F9 | A lost reply mints a new prompt (RP-10) | Client E2E: reply lost with its socket, read back by id on the next socket, `sent == 1`; O3. |
| F10 | A closed connection half-admits (RP-15) | Mid-call drop: the receipt settles by itself, or (never reached) the same id admits it once. |
| F11 | Racing first attempts double-admit | Four concurrent identical sends: one receipt, one run. |
| F12 | A follow-up overtakes worktree setup | `a_new_worktree_holds_…`: queued, provider untouched while setup runs. |
| F13 | Work runs after failed/cancelled setup | Rejected dispatch receipt naming the setup, row held `ExecutionUnavailable`, nothing run; explicit send-now after a successful retry runs once in the worktree. |
| F14 | A stale stop hits a newer turn (RP-14) | Stop fenced by an ended turn: `staleTurn`, the running turn continues; the current stop ends it; another task keeps running. |
| F15 | Queue actions race (RP-13) | Removal at a stale revision: `queueRevisionConflict`, row kept; at the current revision removed and never run. Edit/reorder: limitation. |
| F16 | Accepted work dies with the phone (RP-15) | Messages accepted before a revoke run after it, in order; the revoked runtime ends (`rePair`) and refuses new commands. |
| F17 | Pre-creation input lost or sent early | Draft behind its create, through a phone restart; O4 (held after a refused create). |
| F18 | Delivery lost across suspension/restart | Saved outbox restored for the same host/session only; an uncertain entry is read back after restart, never resent; O1. |
| F19 | Delivery depends on an open view | The reconciler belongs to the runtime: delivery is followed with no view (client E2E). |
| F20 | Live controls replayed offline | `stopTask` without a socket is `notOpen`, nothing queued; O5. |
| F21 | Echo and host diverge (RP-20) | Host transcript shows each message once, in order, before and after a resnapshot; O2/O3/O9 refuse foreign, late and wrong-message receipts. |
| F22 | An older Mac is commanded | Without `remote-task-control-v1` the outbox is not delivered (`outboxIssue`), live controls are `unsupported`. |
| F23 | An approval card appears in Full/Auto | No approval or input intent exists remotely (RM-05). |
| F24 | A phone-created task falls back to legacy Ask | Without a mode: Auto recorded for a provider with Auto; `permissionModeRequired` receipt, no task, for OpenCode; explicit `ask` is `bad params`; Full recorded on request (`a_paired_session_…`). |

Outbox table (`luna-sdk/tests/outbox.rs`): O1 saved outbox only for its host
and session, whole and bounded; O2 receipts for another task/session/payload
refused; O3 only host receipts settle, never regress; O4 per-task order,
draft, held, release by discard; O5 live controls never sent late; O6 discard
only what the host does not own; O7 per-intent frozen fences; O8 bounds and
pruning; O9 delivery follows only its own dispatch receipt; O10 an unadmitted
entry is resent unchanged, a refusal settles without a receipt.

## RP mapping

- **RP-10:** F6, F7, F9, F11 (host and client E2E).
- **RP-11:** forged rows are owned by `remote_doc_authority.rs`
  (`foreign_rows_cannot_run_from_an_open_chat_or_replay`, unchanged); the
  legitimate paired RPC creating its own receipt is F2/F6 above.
- **RP-12:** F1, F3, F4.
- **RP-13:** F15 (removal); edit and reorder are not remote intents yet.
- **RP-14:** F14.
- **RP-15:** F10, F16.
- **RP-20:** F21.

## Criteria mapping

- **admission:** `RemotePrincipal` + `handle_remote`, `remote_commands.rs`
  steps 1–6, identity as `payloadHash`; F1–F8, F10, F11.
- **outbox:** `Outbox`, the runtime reconciler, `restore_outbox`; same id →
  original receipt (F6), changed payload fails (F7), lost reply read back
  (F9), drafts (F17), queue behind setup (F12), hold after failed/cancelled
  setup (F13), suspension/restart (F18, F19).
- **verification:** the three host tests, the client campaign, the outbox
  table and the native campaign with real Codex/Claude Code turns. IOS-03
  owns the native composer and queue UX and remains required by RM-07.
- **handoff:** the orchestrator's runs and evidence below; `luna-engine`,
  `luna-ui` and the mobile core changed, so LUNA Dev needs a refresh and the
  iOS bindings a regeneration.

## Commands (orchestrator)

**1. Generated contracts** (a catalog row, a scope and an SDK operation were
added; `operations.json` and `operation-catalog.json` hunks were written by
hand to match):

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
scripts/cargo.sh check --locked -p luna-ui -p luna
scripts/cargo.sh clippy --locked -p luna-rpc -p luna-sdk -p luna-mobile -p luna-engine --all-targets
```

**3. Tests.**

```sh
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-rpc --features schema --test contract
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-rpc remote_auth
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-sdk --features schema --test generated
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-sdk --test outbox --test projection_store
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test remote_task_commands -- --test-threads=1
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test message_queue --test project_commands_worktree_setup --test host_capacity --test remote_doc_authority --test worktree_on_run
CARGO_INCREMENTAL=0 LUNA_REMOTE_TEST_IP=<this Mac's Wi-Fi IP> scripts/cargo.sh test --locked \
  -p luna-engine --test remote_access gateway:: -- --test-threads=1
npm run typecheck --prefix sdk/typescript   # if the TS package has the script
```

Expected: all pass. `remote_task_commands` re-runs its setup case in a child
with a disposable `LUNA_WORKTREES_DIR` (about 10–20 s); the gateway campaign
`mobile_outbox_…` takes about 15–40 s and prints SKIPPED without
`LUNA_REMOTE_TEST_IP` on a Mac whose default route is not reachable. The
second line re-runs the queue/setup/capacity owners the queue gate touches.

**4. Bindings.** `FACADE_VERSION` is 3: run `scripts/ios/build-mobile-core.sh`
as IOS-00 step 2 describes (regenerate and commit `Luna/Generated/luna_core.swift`
and `RemoteProbe/Generated`). The new Swift calls are additive:
`createTask(projectId:permissionMode:)` (`TaskPermissionMode` `.full`/`.auto`), `sendMessage(taskId:text:steer:newWorktree:worktreeBase:cancel:)`,
`stopTask(taskId:cancel:)`, `removeQueued(taskId:messageId:expectedQueueRevision:)`,
`sendQueuedNow(…cancel:)`, `steerQueuedNow(…cancel:)`,
`setTaskArchived(taskId:archived:)`, `cancelCapacityWait(taskId:commandId:)`,
`outbox()`, `outboxRevision()`, `outboxIssue()`, `outboxBytes()`,
`restoreOutbox(bytes:)`, `discardOutboxEntry(commandId:)`, records
`OutboxEntryRecord`, enums `OutboxEntryStatus`, `OutboxDeliveryState`.
Existing Swift is unchanged.

**5. LUNA Dev.** `scripts/run-macos-dev.sh --refresh`, then the usual process,
port and `codesign --verify --deep --strict` checks.

## Native QA (Simulator, QA LUNA app, real providers)

The host is a QA LUNA app with all four `LUNA_DEV_*` overrides
(docs/reference/qa-infrastructure.md), never the primary profile.

1. In the QA app: rename this device to `RM04 QA Mac` (Settings → Devices),
   add a project on a disposable git repository, and create one task per
   provider to check (Codex, Claude Code) from the desktop composer without
   sending anything; note the project id and task ids (`luna --port <QA IPC>
   inspect` or the QA app's diagnostics).
2. Settings → Connections → Control this Mac: enable on the Mac's Wi-Fi
   address, Add, **Copy pairing code** (the sheet shows "Read tasks, Operate
   tasks" scopes).
3. Build for testing (IOS-00 step 2, `DD=../../target/ios-dd/rm04`), install
   `Luna.app` on the booted iPhone 17 Pro Simulator `$SIM`, then write the
   fixture into the app's Documents:
   ```sh
   DOCS="$(xcrun simctl get_app_container "$SIM" com.novnski.luna.ios data)/Documents"
   cat > "$DOCS/rm04-commands.json" <<EOF
   {"code": "<copied luna://pair code>", "name": "RM04 QA Mac",
    "projectId": "<project id>", "providerTaskIds": ["<codex task>", "<claude task>"],
    "permissionMode": "full"}
   EOF
   chmod 600 "$DOCS/rm04-commands.json"
   ```
   Omit `permissionMode` to check the Auto default (the QA host's provider
   and model must offer Auto, or the create is refused with
   `permissionModeRequired`).
4. Run:
   ```sh
   xcodebuild test-without-building -project apps/ios/Luna.xcodeproj -scheme Luna \
     -destination "platform=iOS Simulator,id=$SIM" -derivedDataPath target/ios-dd/rm04 \
     -parallel-testing-enabled NO -only-testing:LunaTests/RemoteTaskCommandsTests \
     -resultBundlePath target/qa/rm-04/<UTC stamp>/native/RemoteTaskCommandsTests.xcresult
   ```
5. Expected: 1 passed; `rm04-native-result.json` has `draftBeforeCreate`,
   `stopRefusedOffline`, `outboxRestored`, `firstMessageOnce`, `neverResent`
   true, each `providerTasks` entry `messageOnce: true` with its provider id,
   and every command `applied` (the stop `applied` or `pending`) with
   `sends` ≤ 1. On the Mac, the created task shows the first message once,
   the long turn stopped, and each provider task one phone message and its
   answer. Copy `rm04-native-result.json`, hash it, and delete the fixture.
6. Afterwards revoke the phone's session (Connections → Revoke, or
   `luna --port <QA IPC> remote sessions` / `remote revoke-session <id>`),
   quit the QA app gracefully and erase only `$SIM`.

## Limitations and open questions

- **Queue edit and reorder are not remote intents.** The desktop's lease
  based edit (`Begin/Finish…QueuedMessageEdit`) and `MoveQueuedMessage` need
  their own remote shapes; RM-04 covers remove, send-now and steer-now with
  revision fences. IOS-03 decides the phone's queue editing UX.
- **Phone-created tasks use the host's provider for new tasks**; provider
  and model pickers are RM-05. Their permission mode is Full or Auto only
  (never legacy Ask); a provider and model without Auto need an explicit
  Full (`permissionModeRequired` otherwise).
- **Delivery tracking reads the dispatch receipt of an unedited row**
  (`…:1`); a row edited on the desktop, or delivered by an explicit
  send-now, shows `queued` until the transcript projection shows it.
- **The admission lock is per process**, which is the profile's one engine
  owner; across a restart the durable receipt makes a retry its receipt.
- **Native QA cannot drop one reply on a real socket**; lost replies are
  exercised by the Rust gateway campaign (recording provider), and natively
  through suspension and restart from the saved outbox.
- **The outbox is saved by the platform** when its revision moves; a crash
  between a send and that save loses the local entry (the Mac's receipt and
  transcript keep the truth). IOS-03 wires the save into the app's lifecycle.
- **Steer is fenced by the turn the user saw**: if that turn ended or
  changed before admission the steer is refused as `staleTurn`, never
  delivered to a newer turn or turned into a new one. A plain send to an
  idle task starts the next turn.
