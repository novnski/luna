# CORE-05 typed SDKs and unified CLI operations

Source before the change: `3c959fb3` (13 October 2026). Live status belongs to
`pending_tasks.py`. This is the contract and the evidence plan; results are
recorded here once the orchestrator has run them.

## Baseline (what CORE-01/03 left)

- One reviewed catalog (`luna_rpc::operations`, 251 rows) with ids, kinds,
  reply shapes and authority; fail-closed dispatch; command admission (CORE-03).
- Schemas only for frames, the command envelope, receipts and ids. No
  per-operation types, no typed error envelope, no SDK package, no discovery
  RPC. `RpcClient::call` takes a method string and JSON.
- The CLI was read only (`luna inspect`). The desktop app built
  `ExecuteTaskCommand` envelopes, fences and lost-reply recovery by hand in
  `crates/ui/src/task_commands.rs`.
- `WatchTaskCommands` read its snapshot *before* subscribing, so a receipt
  committed in between was never streamed; a lagged stream dropped receipts
  silently.

## What changed

- **`luna-sdk` (`crates/sdk`, version 0.1.0, API 1).** Depends on `luna-proto`
  and `luna-rpc` only, never the engine. Typed operations (`ops`):
  `describeApi`, `engineInfo`, `getTaskCommandSnapshot`,
  `getTaskCommandReceipt`, `executeTaskCommand` (published intent:
  `rename`), `watchTaskCommands`. `Client` adds negotiation, deadlines,
  validation and the command contract; `TaskCommandWatch` adds disposal and
  cursor repair.
- **Generated, CI-checked bindings.** The Rust types generate
  `crates/sdk/contract/v1/sdk.json` (operations, intents and their fences,
  error kinds, frames, watch update, `$defs`) and
  `sdk/typescript/src/generated.ts` (the same manifest plus TypeScript types).
  `tests/generated.rs` fails on drift and on a package version mismatch.
- **`@luna/sdk` (`sdk/typescript`).** A hand-written runtime driven only by the
  generated manifest: it validates every request and reply against the
  generated schemas (`validate.ts`) and mirrors the Rust contract below.
- **`DescribeApi`** (`luna.core/describe-api@1`, query, local, owner only):
  the API version, engine build and every served operation. Its result schema
  is published in `crates/rpc/contract/v1/schemas/api-description.json`.
- **CLI.** `luna inspect api|receipt|watch-commands` (reads through the SDK)
  and `luna task rename` (the first deliberate command). Existing inspect
  commands and their JSON are unchanged.
- **The app's task commands moved onto the SDK boundary.** Every desktop task
  command now goes through `luna_sdk::Client::execute`; the rename use case is
  sent as the SDK's typed `TaskIntent::Rename`, with the SDK's fences, so the
  app, the CLI and both SDKs send the same command. The view code keeps its
  retry wording and its one retry of a reversible stale-revision rename.
- **Engine.** `WatchTaskCommands` subscribes before it reads (a receipt can
  now arrive twice, which a cursor drops, but never zero times).

## Contract

**Versioning.**
- The SDK version (`SDK_VERSION`, `@luna/sdk` version) is independent of the
  app version. `API_VERSION` is the catalog version (1); each operation also
  carries its catalog `version`.
- `negotiate()` calls `DescribeApi`. A different `apiVersion` is refused
  (`incompatible`). An operation missing from the engine is `unsupported`,
  and a different operation version is `incompatible`, both before sending.
- An engine older than `DescribeApi` answers `unknown method`; it is accepted
  as API 1 without a description and each call relies on the engine's own
  `unknown method`. In-app clients do not negotiate (same build).

**Runtime validation.** Rust decodes every reply into the types the schemas
are generated from; TypeScript validates against those schemas (and its
requests too). Both refuse a receipt for another command or task, a snapshot
for another task, and event pages that are out of order. Unknown fields are
kept compatible; unknown enum values (a new receipt status or reason code)
fail closed as `invalidResponse` in both, never as success.

**Errors** (`SdkError::kind`, shared by both SDKs). A domain refusal is a
receipt (`conflict`, `rejected`, `superseded`, `failed`), not an error.

| Kind | Meaning | CLI exit / kind |
| --- | --- | --- |
| `offline`, `timeout` | No reply. Reads are safe to retry. | 3 `offline` |
| `unsupported` | The engine does not serve the operation. | 6 `unavailable` |
| `incompatible` | Another API or operation version. | 12 `incompatible` |
| `invalidRequest`, `invalidResponse` | `bad params`, or a reply that breaks the schema. | 7 `malformedResponse` |
| `engine` | The handler failed. | 8 `engineError` |
| `unknownOutcome` | The command may have been admitted; its receipt could not be read. | 9 `unknownOutcome` (`data.commandId`) |
| receipt not `applied` | Refused, or still `pending` after 10 s. | 10 `commandRefused` / 11 `commandPending`, with `data.receipt` |

**Commands and unknown outcome.**
- The command id is minted once, before the first send. The target task is
  explicit; `targetDeviceId` is never inferred (the CLI also requires the
  exact machine, environment and project, as `luna inspect` does).
- Fences come from the task's current snapshot (always read first, as the app
  did) or from an exact reviewed snapshot. Rename carries the revision and
  binding-generation fences.
- On any send failure: read the receipt by the same id; if there is one, it is
  the result. If there is none, send the identical envelope once more (the
  engine deduplicates), then read again. No receipt after that means not
  admitted (the first error is returned). A failed read is `unknownOutcome`.
  A new command id is never used for a retry.
- `pending` is not success: `wait_for_terminal`/`waitForTerminal` poll the
  receipt; the CLI reports a still-pending command with exit 11.

**Watches.**
- The first item is the snapshot. With a cursor, the bounded first page is
  completed from `GetTaskCommandSnapshot` before anything live is delivered.
- Terminal receipts carry their durable task sequence. The cursor is the last
  one delivered. A sequence at or below the cursor is a duplicate and is
  dropped; one that skips ahead is preceded by the missing events read from the
  durable page; `reloadRequired` becomes a `reload` update with the snapshot.
- Receipts without a durable position (accepted, pending) are `progress` and
  do not move the cursor. `resync()` reads past the cursor on demand (after a
  reconnect, or when no later receipt would reveal a lagged one).
- Dropping (Rust) or `dispose()` (TypeScript) cancels the engine stream at once;
  the connection stays usable. Queues are bounded (256); a stalled consumer
  loses the stream and resubscribes from its cursor.

## Failure contract (before code) and owners

| # | Failure | Owner |
| --- | --- | --- |
| F1 | A CLI or SDK client is allowed what another client is refused. | `sdk_parity` allowed/denied: identical status, reason and payload hash; each receipt equals the engine's durable event |
| F2 | A target is inferred (focus, project) and the wrong task changes. | `sdk_parity`: task outside the named project → exit 5, no new event |
| F3 | A lost reply is resent blindly or under a new id; duplicate side effect. | `sdk_parity` reply loss: one `ExecuteTaskCommand`, ≥1 receipt read, one event, revision +1 (Rust SDK, CLI, TypeScript) |
| F4 | JSON that is not a receipt decodes as one. | `sdk_parity` corrupt receipt → `invalidResponse` (Rust, TypeScript), CLI exit 7 |
| F5 | A watch skips an event (lag, start race). | `sdk_parity` hidden receipt: repaired in order before the revealing receipt (Rust, TypeScript) |
| F6 | A disposed watch keeps its engine stream. | `sdk_parity` engine-side stream lease returns to 0 after drop/dispose with the client still connected, and after the CLI exits |
| F7 | An engine with another API version is driven anyway. | `sdk_parity`: `apiVersion` 2 → CLI exit 12, SDK `incompatible`, no event |
| F8 | TypeScript drifts from Rust. | `luna-sdk --test generated` (CI) |
| F9 | Inspect JSON changes. | Existing `luna --test inspect_cli`, unchanged |
| F10 | Moving the app onto the SDK changes its envelopes or recovery. | Existing UI fixture suites (`composer`, `queue`, `goals_e2e`) that inspect envelopes and lost replies |
| F11 | A new method bypasses policy. | `DescribeApi` is `localOnly`; CORE-01 `contract` and `remote_auth` inventory tests |
| F12 | `pending` reported as success. | Contract only: no deterministic pending rename exists (see limitations) |

## Criteria

- **contract:** this document; `crates/sdk/src/{ops,client,watch,error}.rs`.
- **delivery:** `crates/sdk`, `sdk/typescript`, `DescribeApi`, the CLI
  commands, and the app's task commands on `luna_sdk::Client::execute`
  (`crates/ui/src/task_commands.rs`), with no engine import in either SDK.
- **verification:** `apps/luna/tests/sdk_parity.rs` (E03/E07), the drift test,
  and the unchanged inspect suite.
- **handoff:** the orchestrator's build/E2E/lint/review/app refresh/delivery.

## Commands (orchestrator)

Update `Cargo.lock` first (new `luna-sdk` package; `luna-ui` and `luna` depend
on it; `luna` gains the `futures` dev-dependency). Then regenerate, in order:

```sh
scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract
scripts/cargo.sh run --locked -p luna-sdk --features schema --example generate
```

Checks and tests:

```sh
scripts/cargo.sh check --locked -p luna-sdk --all-targets
scripts/cargo.sh check --locked -p luna-sdk --features schema --all-targets
scripts/cargo.sh test --locked -p luna-rpc --features schema --test contract
scripts/cargo.sh test --locked -p luna-rpc
scripts/cargo.sh test --locked -p luna-sdk --features schema --test generated
scripts/cargo.sh test --locked -p luna --test sdk_parity
LUNA_NODE="$(command -v node)" scripts/cargo.sh test --locked -p luna --test sdk_parity -- --ignored
scripts/cargo.sh test --locked -p luna --test inspect_cli
scripts/cargo.sh test --locked -p luna-engine --test task_commands --test remote_auth
scripts/cargo.sh test --locked -p luna-ui --lib -- --test-threads=1
npm install --prefix sdk/typescript   # dev dependencies only: typescript, @types/node
npm run typecheck --prefix sdk/typescript
```

The ignored test needs Node.js 23.6 or later (it runs `.ts` directly; 24.x is
installed). Expected: every test passes; the drift tests are clean after
regeneration.

External example against a disposable headless engine (never the user's data):
`scripts/cargo.sh run --locked -p luna-sdk --example rename_task -- <port> <task> <title>`
and `node sdk/typescript/examples/rename-task.ts <port> <task> <title>`.

## Native QA (LUNA Dev)

On a disposable project and task in LUNA Dev:
1. Rename the task from the sidebar menu (Rename).
2. `luna inspect --json read --machine M --environment M --project P --task T`
   shows that rename's receipt: `intent` `rename`, `status` `applied`,
   `clientId` = the device id, revision advanced by one.
3. `luna task --json rename ... --title "<same title>"` returns `applied` with
   the same `payloadHash` as the app's receipt and `clientId` `luna-cli`.
4. `luna task --json rename ... --expected-revision 0 --title X` exits 10 with a
   `staleTaskRevision` receipt; the sidebar title is unchanged.
5. `luna inspect --json watch-commands ... --after-task-sequence 0 --count 1`
   lists the same receipts; `luna inspect --json api` shows every SDK
   operation served.

## Limitations

- One published intent (rename). Other app intents stay first-party values
  sent through the same boundary; publishing them is later work.
- Discovery is owner IPC only; RM-03 decides remote discovery scope.
- `pending` → terminal is not exercised deterministically (a mock rename is
  applied synchronously); the CLI's exit 11 path is untested.
- The SDK does not verify the local data-root claim; the CLI still does when
  no `--port` is given.
- The TypeScript package runs `.ts` with Node type stripping and is not
  published to a registry; a bundler or Node ≥ 23.6 is required.
- A JSON number such as `1.0` passes the TypeScript integer check but not
  serde; no engine emits one.
- Watch streams still survive engine shutdown (CORE-03 deferral); a client sees
  the stream end only when the connection closes.
