# Persistent fork verification — 2026-10-04

This milestone follows workbench commit `0800dfc6`. Evidence is under
`target/qa/persistent-fork-threads/`; earlier native workbench evidence is
preserved unchanged under `target/qa/super-chat-workbench/native-workbench-final/`.

## Engine

62 focused integration tests pass:

- `pane_chat` (11): ordinary fork execution and settings, Codex/Claude
  provider contracts, nested forks, children, source deletion, ClearContext,
  explicit provider changes and mismatched binding refusal.
- `recovery` (27): accepted and rejected continuation, restart identity,
  normal command recovery and preservation of legacy no-replay receipts.
- `side_chats` (16): all materialized legacy states, repeated startup,
  view close during a live turn, independent source lifecycle, nested forks,
  imported history without a document snapshot and missing-binding refusal.
- `task_commands side_chat` (2): native creation identity and uncertain
  creation receipts cannot replay provider mutations.
- `chat_workbench` (6): validation, revisions, deduplication and immediate
  layout-watch pruning when a chat is deleted. A failing-before trace
  reproduced the stale revision conflict before the watch fix.

```sh
FORK_EVIDENCE_DIR="$PWD/target/qa/persistent-fork-threads/engine" \
PANE_ENGINE_EVIDENCE_DIR="$PWD/target/qa/persistent-fork-threads/engine" \
  scripts/cargo.sh test --locked -p luna-engine \
  --test pane_chat --test recovery --test side_chats
scripts/cargo.sh test --locked -p luna-engine --test task_commands side_chat
scripts/cargo.sh test --locked -p luna-engine --test chat_workbench
```

The real native acceptance reuses the previously ended Codex fork rather
than creating another one. Its verification checks every prior message
(including parts, status and timestamps), its exact managed binding and
native provider ID, and a new provider response. The script also validates
the signed QA bundle, IPC owner and isolated data directory.

## UI and appearance

Run one GUI fixture at a time, with the primary app stopped gracefully:

```sh
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
  --example chat-workbench-fixture --example side-chat-pane-fixture \
  --example related-chats-fixture --example chat-placement-fixture \
  --example composer-parity-fixture --example appearance-audit
target/debug/examples/chat-workbench-fixture ABSOLUTE_EVIDENCE_DIR
target/debug/examples/side-chat-pane-fixture ABSOLUTE_EVIDENCE_DIR
target/debug/examples/related-chats-fixture ABSOLUTE_EVIDENCE_DIR
target/debug/examples/chat-placement-fixture ABSOLUTE_EVIDENCE_DIR
target/debug/examples/composer-parity-fixture ABSOLUTE_EVIDENCE_DIR
target/debug/examples/appearance-audit ABSOLUTE_EVIDENCE_DIR
scripts/cargo.sh test --locked -p luna-ui --lib -- --test-threads=1
```

The existing UI suite passes all 1,185 tests. The initial parallel run
aborted in the native font tests; the configured serial invocation passes.
No new unit tests were added.

Final passing fixture outputs: `ui-attempt-8`, `composer-attempt-2`,
`placement-attempt-2`, `related-attempt-1`, `side-attempt-2` and
`appearance-attempt-2`. The workbench run includes both final header
refinements. The 144 rendered pairs pass with minima of 4.68:1 for text and
3.11:1 for the measured non-text role. The new focused-tab close glyph also
passes at rest and on hover (3.40:1 minimum on hover at the default accents).

The appearance matrix passes 810 theme/surface/accent combinations. It
measures text at 4.5:1 and meaningful non-text controls at 3:1, including
terminal colors. The workbench fixture also measures rendered surface
pixels across representative themes and checks the contained frame,
overflow direction, selected-tab fills and keyboard routing. Selected tabs
have no underline or outline; the focused group's fill is a soft accent tint.
See the full
[appearance audit](../chat-workbench/appearance-audit.md) for scope and limits.

## Boundaries

- Synthetic provider fixtures exercise both native-fork contracts; the real
  provider acceptance uses Codex. It does not claim a new live Claude run.
- Imported history without a managed provider binding remains visible and
  fails Send with an actionable recovery error. It is never silently rebound.
- A native fork requires an exact inherited checkpoint mapping. If unavailable,
  creation fails before provider mutation and offers the snapshot alternative.
- Contrast measurements do not establish full accessibility conformance.
  VoiceOver and arbitrary user-supplied palettes were not audited here.

## Native acceptance

The original ended fork `61c93534-7758-4cc8-8a19-ce89ca585731` became active
with no expiry. It returned `PERSISTENT_FORK_RESUME_QA_OK` through its ordinary
composer, then survived tab close/reopen and a real process restart. Every
original message is unchanged. Its binding remains
`2b77bc8c-5a00-445c-9e4d-d1606898f2d3`, generation 1, pointing to the original
Codex thread `01a106f2-7bcb-7ed1-bb91-a82a301c8fbe`.

`native/verified-persistent-forks.json` cross-checks the public identity with
the read-only SQL identity ledger. `native/verified-restart.json` compares
the complete saved layout, child drafts, messages and binding IDs across
different application PIDs. The second child remains unstarted with draft
`WORKBENCH_DRAFT_REOPEN_QA`. A fork also opens as its own workbench Main.
Startup retains LUNA's existing most-recent-active-chat landing policy;
opening QA Main restores its saved five-tab split workbench.

To repeat against the preserved QA profile, first stop any running LUNA app
gracefully, then use all four identity overrides:

```sh
LUNA_DEV_APP_NAME='LUNA QA Workbench' \
LUNA_DEV_BUNDLE_ID=com.novnski.luna.qa.workbench \
LUNA_DEV_IPC_PORT=59805 \
LUNA_DEV_DATA_DIR="$PWD/target/qa/super-chat-workbench/native/data" \
  scripts/run-macos-dev.sh --refresh
node scripts/qa-chat-workbench.mjs verify-forks --persistent-forks
node scripts/qa-chat-workbench.mjs checkpoint-restart --persistent-forks
# Quit that exact QA app gracefully, then launch the same bundle again.
node scripts/qa-chat-workbench.mjs verify-restart --persistent-forks
```

Do not reseed or replace the original fork. Dark/light native captures show
the real provider response, contained canvas and header refinements. The
final selected-tab treatment is captured in `n08-final-tab-fill-dark.png`
and `n09-final-tab-fill-light.png`.

## Handoff and evidence integrity

The final incremental app compilation took 17.92 seconds. The primary
`LUNA Dev.app` refresh took 3 seconds and launched PID 76106 on IPC 59777.
Its exact executable path, listener and deep/strict code signature passed;
its Mach-O build UUID matches `target/debug/luna`. It uses the verified QA
source; the primary and QA bundle identities produce different signatures.
Details are in
`primary-handoff.json`.

The disposable QA bundle was quit gracefully, unregistered and removed after
hashing native evidence and verifying that nothing held its files open.
Its original data and the shared debug cache remain. Task-created fixture
runtimes were archived and hashed before removal; cleanup records are in
`cleanup/`. All 167 entries in the earlier workbench milestone's hash list
still verify. This milestone's `SHA256SUMS` covers its retained evidence,
verification logs and feature patch.
