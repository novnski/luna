# Idle native archive lifecycle follow-up

Source base: combined audit `8169b69219f5d1b598f64189a8e2b1b937aa3d7c`
(this branch additionally contains the report-only `446b7843`).

## Failure matrix recorded before implementation

- An idle completed Codex turn retains its warm app-server and exclusive native
  writer. Archive must release that target writer before the separate metadata
  client invokes `thread/archive`, including delayed child teardown.
- An active turn, pending steer, approval, or input remains protected by the
  current active-turn policy. Archiving must not interrupt it opportunistically.
- A second warm task must retain its writer and runtime. Dispatch into the
  selected task must be excluded until archive and the local durable commit end.
- Explicit provider failure or a lost response must not publish local archive
  success. Same-command retries must not replay the provider mutation, including
  after restart. A deliberate new retry after a definite rejection may succeed.
- Native binding/session identity and completed transcript survive retirement
  and restart. No fresh native task is created as an archive workaround.
- A writer that cannot be released before the bounded deadline must prevent
  the archive call; local state must remain unarchived and the failure visible.
- Concurrency follow-up before its correction: automatic queue drain can claim
  and prepare a request while native archive holds the task transition gate.
  Releasing that gate after local archive commits must not start this stale
  request. Standard new RPC commands reread state under the task-command lane;
  the dispatch boundary also needs a fresh archived-state check for work that
  was accepted earlier or originated in automatic drain. A newly accepted user
  send may still explicitly unarchive through its existing path.

Observed native symptom: provider audit `native/archive-before-restart/` holds
idle snapshots with no `activeTurnId`, but archive replies report Codex error
`-32600`, `already has an active writer`. Graceful app restart was a cleanup
workaround, not a source fix. The planned regression uses actual EngineCore/RPC,
a persistent synthetic Codex writer and an injected metadata subprocess that
enforces the same exclusive-writer condition. It never reads real credentials.

Planned production change: a per-task transition guard closes the idle steering
mailbox using the adapter's existing graceful idle-exit contract and waits for
the stream to finish before archive. The guard remains held through native
metadata mutation and local commit. A generic interrupt alone is insufficient
proof of writer release because terminal Done can precede child teardown.

## Authentic baseline

Artifacts are retained under `target/qa/idle-archive-2026-09-26/`.
The unchanged-runtime baseline compiled and ran four integration cases:
**one passed, three failed**. The active-turn case passed. Idle archive returned
Pending/ProviderAcceptanceUnknown with the same `thread/archive` **-32600**
`already has an active writer` error as native QA; the target writer remained
open, and the stuck-writer case proved a native mutation was attempted before
release. See `baseline.log`, `baseline.exit`, and the copied native symptom
receipt/snapshot.

An earlier setup-only run used human-readable task IDs, which deliberately
remain legacy/unmanaged in this engine. The warm session itself completed, but
the identity-ready fixture condition could not pass. Those diagnostics remain
in `baseline-fixture-setup.log` and `setup-diagnostic.log`. Replacing only fixture
task IDs with UUIDs reached the real native-archive assertion; no production
change was used to repair the fixture.

## Prepared correction

`SessionsEngine::prepare_native_archive` holds only the selected task's existing
transition gate. It refuses active/pending turns, closes the target's warm
mailbox, and waits at most five seconds for both positive idle-stream EOF
evidence and removal of that same run. Error/terminal-event removal alone is
insufficient. Codex's existing mailbox-closed path waits for child shutdown
before ending its stream. The idle EOF/finalization path uses the runs/status
mutexes and does not reacquire the task transition gate, avoiding a lock cycle.
The RPC retains this gate through reconciliation, native mutation, and local
commit, without replacing the native binding or clearing resume identity.

Production changes are confined to `sessions.rs` and the `SetArchived` branch
in `rpc.rs`. The integration fixture uses EngineCore/RPC, a persistent synthetic
writer with delayed teardown, and an actual injected metadata subprocess.
Fixed validation and real post-fix Codex idle-to-archive verification remain
pending the coordinator's next explicit slots. The build slot was released
after baseline while the source correction was prepared.

The first fixed batch passed all four archive cases in 5.49 seconds. Review of
the waiting-dispatch boundary found no archive-state check after the transition
gate. A fifth integration case pauses the real metadata subprocess after writer
teardown, makes the actual queue drain claim and wait, and completes archive.
Before the freshness correction it reached the intended assertion and failed:
**a request prepared before archive started afterward** (exit 101, 0.65 seconds).
See `race-before.log` and `race-before.exit`.

`dispatch_inner` now reads the committed chat row immediately after obtaining
the task transition gate and refuses archived work before account/provider or
resume dispatch. The fixed race preserves the original transcript, keeps the
task archived, starts no new runtime/writer and retains the uncertain queue row
for explicit review. Standard RPC admission already holds the task-command
lane while rereading the snapshot; this extra check covers automatic drain and
previously accepted commands waiting below that admission boundary. A fresh
deliberate send still unarchives through the existing acceptance path; its
existing engine E2E passes. The EOF cleanup path never reacquires the task gate.

## Focused verification

**47 passed, zero failed**, with one opt-in real Claude test ignored:

| Suite | Passed |
| --- | ---: |
| Idle archive and concurrency | 5 |
| Restart/resume | 8 |
| Durable task commands | 33 |
| Explicit send unarchives | 1 |

Engine compile and Clippy (`--lib --test idle_archive`) passed; warning output
is retained. Formatting and diff whitespace pass. `verify-fixed.sh` in the
artifact directory records repeatable commands, logs, exit codes, durations,
cache results and cleans only its disposable fixture root. All Cargo invocations
use `scripts/cargo.sh`, `--locked` and `CARGO_INCREMENTAL=0`.

The build slot was released after this batch. The source/test/report checkpoint
is ready for immutable coordinator review. Native rebuilding and real Codex
idle-to-archive without app restart remain explicitly pending a separate grant.
The subprocess fixture establishes the engine ordering and failure semantics;
it does not substitute for the required real adapter teardown check.
