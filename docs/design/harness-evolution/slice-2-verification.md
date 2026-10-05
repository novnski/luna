# Portable context verification

Slice 2 extends the existing command and context-boundary transactions with
host previews, local captured payloads and bounded shared receipts. Provider
switching, a new independent task with context, and findings sent to a fork's
source all require an explicit choice. Applying a preview starts no turn.

The destination's next send carries the captured text. Its visible user message
and sidebar preview retain only the user's submitted prompt. Native acceptance
settles delivery; a completed matching turn marks the receipt consumed. Unknown
acceptance blocks reinjection. Omitted visible text can be read in checked UTF-8
segments while the unchanged source remains available.

## Reproducible checks

Evidence: `target/qa/harness-evolution/slice-2/`. Red runs and their corrections
are retained alongside green runs; they are not replaced by passing output.

- Eight engine E2Es exercise RPC, durable commands, documents, SQLite and
  provider requests: stale previews, atomic rollback, duplicate apply,
  uncertain acceptance across restart, source deletion, honest unknown capacity,
  partial UTF-8 deltas, portable task creation and provider switching.
- A warm-provider regression reproduces a real live failure: the old Codex
  process reported its ID during retirement after a fresh Claude binding was
  committed. The legacy resume cache then supplied that ID to Claude. Fresh
  managed bindings now override legacy observations, and stale native IDs cannot
  update the cache. The focused red/green artifacts are in `warm-before-2/` and
  `warm-after/`.
- The production Shell's `composer-parity-fixture` covers the transfer dialogs,
  no preselected provider-switch choice, stale apply, uncertain command outcome,
  lost responses, unchanged drafts, new-task cancellation, connection-generation
  races and omitted-item Read more. `composer-final/` includes actual open-panel
  captures and JSON readback. Early fixture failures exposed missing synthetic
  session acceptance and task identity; the fixture now uses the real create RPC
  and emits session acceptance.
- The existing 1,185 UI checks passed with serial execution. Affected-crate
  Clippy, formatting, script syntax and diff checks passed. Existing warnings
  remain; no new unit tests were added.
- After the resume fix, all 34 queue, 27 recovery, 36 command and eight restart
  checks passed; one existing restart case remains ignored.

```sh
LUNA_CONTEXT_TRANSFER_EVIDENCE="$PWD/target/qa/harness-evolution/slice-2/repeat" \
  scripts/cargo.sh test --locked -p luna-engine --test context_transfer
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
  --example composer-parity-fixture
# Quit any running Luna app gracefully before this GUI fixture.
target/debug/examples/composer-parity-fixture \
  "$PWD/target/qa/harness-evolution/slice-2/composer-repeat"
```

## Native acceptance

`scripts/qa-context-transfer-live.mjs` verifies the exact signed QA bundle,
process, IPC port and data directory before using the engine. It resolves live
model catalogs and preserves every attempted request. No uncertain send is
retried automatically.

The successful `native/run-IlrR3n/` run used Codex and Claude's real native
adapters. A native Codex fork recalled the parent's unique marker and produced
its own second marker. Saving findings while the parent was executing a bounded
sleep did not interrupt it or change its native identity. Steering was held;
explicitly sending the held message recalled both markers in the parent's
existing native conversation. A repeated identical merge was refused because
there was no unseen text.

Codex → Claude → Codex then created fresh bindings and recalled both markers
without putting either marker into the submitted recall prompts. After a normal
app quit/reopen, the same final native session recalled them again and all three
consumed receipts were byte-for-byte unchanged. User messages remained single
entries containing only the submitted prompt.

User branch refs, the Git index, README and working-tree status were unchanged.
Ordinary turns created internal `refs/luna/checkpoints/` refs, recorded separately.
An initial overly broad all-ref assertion failed on those expected checkpoints;
`finish-readback` verified the already completed native run without sending any
new request. Its original failure artifact is retained. The earlier native
failure `run-60ksTl/` remains evidence of the repaired stale-resume bug.

To repeat, build the disposable app with all four identity overrides:

```sh
LUNA_DEV_APP_NAME='LUNA QA Context' \
LUNA_DEV_BUNDLE_ID=com.novnski.luna.qa.context \
LUNA_DEV_IPC_PORT=59810 \
LUNA_DEV_DATA_DIR="$PWD/target/qa/harness-evolution/slice-2/native/data" \
  scripts/run-macos-dev.sh --refresh
node scripts/qa-context-transfer-live.mjs run
# Gracefully quit and reopen that exact QA app, then:
node scripts/qa-context-transfer-live.mjs verify-restart
```

The first QA build took 99 seconds; the build with the native-resume fix took
21 seconds. Capacity estimates are conservative UTF-8 byte allowances, not a
provider tokenizer. Transferred text never includes hidden reasoning, tool
payloads, attachments or authorization. Native result validation and production
UI interaction validation are separate evidence sets.

Live acceptance is scoped to Codex and Claude. OpenCode is outside the current
verification scope; no native OpenCode continuity is claimed.

The reviewed QA bundle was unregistered and removed after graceful shutdown;
its data is preserved. Seven disposable fixture/project directories were
archived with SHA-256 hashes before removal. The primary LUNA Dev handoff is
recorded in `primary-handoff.json` with its build duration, exact process,
listening port and verified signature. Shared build caches remain intact.
