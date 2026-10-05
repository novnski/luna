# Durable dispatch verification

Failure cases were specified before implementation in [slice-4.md](slice-4.md)
and [slice-4-ui.md](slice-4-ui.md). Evidence is retained under
`target/qa/harness-evolution/slice-4/`.

## Engine boundaries

Four engine/RPC/document/SQLite E2Es use a controlled provider boundary and
disposable account identities. They prove:

- Missing Claude account: zero provider calls, an atomic Rejected receipt and
  Held message with `executionUnavailable`.
- Engine restart plus account repair: the exact row remains held. Canceling and
  saving an edit preserve that requirement. Explicit Send now delivers one
  canonical user message; duplicate command submission does not resend it.
- Send now with an unavailable provider leaves the existing turn running and
  keeps its active turn identity, with no interruption or extra provider call.
- An error after provider entry remains unresolved and cannot replay across
  restart. Its live claim is not confused with proven local rejection.
- A SQLite trigger rejects the rejection transaction: the in-memory document
  never publishes the uncommitted Held projection, and restart/account repair
  still produce zero provider calls.

The retained red runs show the missing-account receipt incorrectly marked as
acceptance unknown and the edit/cancel operation clearing the hold. Initial
fixture setup failures are retained separately and are not product evidence.

All four final cases pass. Existing regressions passed: 34 message queue,
27 recovery, 36 task command, eight context transfer and eight restart cases
(one existing restart case remains ignored). The 34 queue cases ran again
after the final edit-hold repair. This slice uses protocol peers for fault
injection; it does not claim a new live-account failure test.

## Reproduce

From the repository root, use an absolute evidence directory:

```sh
LUNA_PREFLIGHT_EVIDENCE="$PWD/target/qa/harness-evolution/slice-4/repeat" \
  scripts/cargo.sh test --locked -p luna-engine --test queue_preflight -- --test-threads=1
scripts/cargo.sh test --locked -p luna-engine --test message_queue --test recovery \
  --test task_commands --test restart_resume --test context_transfer -- --test-threads=1
```

## Production UI and handoff

The complete production Shell workbench fixture passed with 74 PNG captures
and `workbench-final/readback.json`. Its new case reads the exact row/revision
and production hold label in Child A, proves reconnect sends nothing, then
requires a new Applied `sendQueuedNow` receipt correlated to that row. The
provider sees one message, the queue row disappears, and Main's draft,
transcript and command ledger remain unchanged. The held-row capture was
visually reviewed.

The first run correctly failed its receipt assertion: the old fixture Mock
script emitted text and Done without SessionStarted. Its pending receipt is
preserved in `workbench/last-command-receipts.json`. A fixture-only wrapper now
emits a real start acknowledgement for this one prompt; earlier scenarios keep
their existing peer behavior. The assertion requires Applied, never Pending.
Both disposable fixture profiles were checked for open files, archived and
hashed before scoped removal after their GUI processes exited.

All 1,185 existing UI library checks passed. Clippy passed for engine/doc/UI
and all targets, followed by a focused pass on the final fixture. Rust formatting
and whitespace checks passed. Existing repository warnings remain unchanged.

```sh
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
  --example chat-workbench-fixture
target/debug/examples/chat-workbench-fixture \
  "$PWD/target/qa/harness-evolution/slice-4/workbench-repeat"
```

Only one LUNA GUI may run. Quit the primary app gracefully before this fixture
and refresh it afterward with `scripts/run-macos-dev.sh --refresh`. The saved
handoff artifact records build duration, exact process, IPC listener, signature
and executable hash. The evidence manifest preserves both failed and passed
attempts.

The primary LUNA Dev refresh took 79 seconds. Process 90284 ran the exact
development bundle, listened on IPC 59777 and passed strict deep signature
verification. QA data and the shared build cache were preserved.
