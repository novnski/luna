# CORE-02 test ownership around real boundaries

Source before the change: `5285702d` (11 October 2026). Live status belongs to
`pending_tasks.py`.
- The failure cases were written first: [failures.md](failures.md).
- The 9 October ledgers rebased onto current source:
  [ledger-rebase.md](ledger-rebase.md).

## Decisions (owner-approved)

| Named item (audit follow-ups) | Production callers | Decision | Proof kept |
| --- | --- | --- | --- |
| `engine integration_actions` (module, 11 tests) | None; no ticket adopts its owner-driver contract | **Retired** (≈1,090 lines) | The live integration inventory is owned by `integration_settings`/`claude_mcp` and `tests/claude_mcp_inventory.rs` |
| `doc_host persist_unwired_interrupt_with_id` (9 tests) | None; its "B0a" plan no longer exists | **Retired** | Five tests guarded live projection rules through it. They are **re-homed** on the production approved-queue path (`commit_approved_queue_projection_after_commit`): stale-saver blocking, remote-row blocking until install, remote row before projection, checkpoint/replay cursor, cursor SQL failure. The poisoned-replay check is folded into the existing post-commit keeper; the shutdown test moved verbatim. The remaining three exercised only the dormant API, or duplicated approved-path keepers. |
| `sessions` prospective interrupt and synthetic-archive grants (26 tests, ≈790 production lines) | None; no ticket plans cross-task stop/archive | **Retired** | `generic_answers_cannot_consume_server_owned_luna_approval` guards a live, kind-agnostic rule; it is **re-homed** onto the real `LunaSend` kind. The live `respond_luna_local_ui_approval` keeps its send branch, and any other kind is `Stale` as before. |
| `backup` isolated-root activation (`activate`, `rollback`, `recover_interrupted_restore`) | None. Production restores through the managed offline path; staging already exists in the UI as "activation unavailable pending restore gates" | **Kept** as a recorded owner gap for a future restore-gates ticket | About eight backup round-trip tests use activation as their readback |
| Claude `SessionIntegrationReceipt`, Codex `discover_skill_observations` | None yet | **Assigned to SW-01** (effective tool/skill inventory), named in its plan and in code | Retained as parser contracts on untrusted provider output |
| UI fixture examples (≈50 never compiled; 21 embedded tests never run) | — | **Routed** into the Linux `ui-tests` job | Baseline found one test failing in that unrouted suite (fixed below) |
| `LunaUITests` (14) | — | **Documented local runner** (`apps/ios/README.md`); macOS minutes stay out of CI | Local run: 13/14 (see baseline) |

## Repairs and routing

- **Restored lost keeper.** `luna_doc::commands::processed_commands_are_skipped`
  was lost to a circular cross-lane deletion. With skipping disabled in
  `evaluate_command`, every doc and engine e2e test still passed. The row is
  restored verbatim from history and turns red under that mutation.
- **Baseline repairs in unrouted suites:**
  - `backup_restore` ×5 pinned docs schema v19; main moved to v20 in
    `70707aee`. The tests now compare against the source database's own
    migration version and a fresh store, so the next migration can't strand
    them.
  - `history-operations-fixture::unsupported_task_backfills_…` predated the
    project requirement (`dd75f9a4`). It now registers the same project as
    the fixture's own seed.
- **CI routing (`ui-tests.yml`):**
  - `backup_restore` joins the engine keepers.
  - New step: UI fixture examples compile, and their embedded and
    feature-gated tests run.
  - Each independent suite step now runs even after an earlier step fails.
    On `afd8455b` a harness failure had silently skipped the CORE-01 contract
    test, the RM-00 compile check and every engine keeper.

## Baseline failure ledger

| Failure | Where | Classification | Owner / next step |
| --- | --- | --- | --- |
| `backup_restore` ×5 (schema v19 pin) | Local; was unrouted | Stale assertion after `70707aee` | Repaired here; now routed |
| `history-operations-fixture` ×1 | Local; was unrouted | Stale fixture setup after `dd75f9a4` | Repaired here; now routed |
| `harness claude_goals::resumed_session_seeds_claudes_restored_goal` | GitHub Linux (`afd8455b`) | A real race: the resumed run's verdict could land before the seed read, so the inherited goal was never reported (diagnosed with `aad809ba`) | **Fixed** in `f1dcc78a` (seed as of run start); green on Linux CI |
| `linux-browser`: "blur sample did not cover the checkerboard" | GitHub Linux | Stale menu coordinates after the right-pane header redesign | **Fixed** in `9b6edce2`; green on Linux CI |
| `macos-frame-recovery`: "repeated tab/toolbar tooltip disrupted the live page" | GitHub macOS | Stale hover coordinates after the same redesign; the later preview/HMR step failed because the fixture booted the UI against an empty data root, and the job also exceeded its 45-minute limit | **Fixed** in `9b6edce2` (browser step green), `bd2021a2` (data root), `88d98323` (timeout) |
| `LunaUITests.MobilePolishTests.testSendClearsComposerAndKeepsTheNextDraft` | Local simulator, full and isolated | Deterministic: demo mode refused to queue a send made during a reply, so the draft stayed | **Fixed** after CORE-02 (`AppModel.hostSupportsMessageQueue`); LunaTests 113/113, LunaUITests 14/14 |
| `codex_subagents::parent_turn_end_does_not_claim_an_uncompleted_child_stopped` | GitHub Linux, first time its step ran | Asserted the child transcript before it published | **Fixed** in `f7b76be8` |
| `child-activity-fixture` compile, `task-organization-fixture` opt-in test | GitHub Linux, first time routed | `libc` only a macOS dependency; hard-coded `/private/tmp` | **Fixed** in `1219c58c`, `a19deb8c` |
| `LunaTests.TranscriptLayoutTests.testToolToggleReversalWhileStreamingKeepsTailAttached` | GitHub macOS, once (passed before and after) | Timing-sensitive: 16 ms sleeps stand in for frames | Open; flagged as a separate task |

Nothing is quarantined: each open failure keeps failing loudly where it runs.

## Evidence (E04)

**Before and after**, same flags (`CARGO_INCREMENTAL=0`; engine and doc with
`CARGO_PROFILE_DEV_DEBUG=0`). The per-build 25 GiB growth guard stopped a
full-debug engine run at 25.28 GiB, as it did in the 9 October campaign.
Full counts are in [suite counts](evidence/suite-counts.json).

| Suite | Before (`5285702d`) | After |
| --- | --- | --- |
| `luna-engine` (102 binaries) | 1,001 passed, **5 failed**, 12 ignored | **965 passed, 0 failed**, 12 ignored |
| `luna-harness` | 338 passed, 0 failed, 12 ignored | 338 passed, 0 failed, 12 ignored |
| `luna-doc` | — | 99 passed (with the restored Skip row) |
| UI fixture examples, compile | ok | ok |
| UI fixture embedded tests (6 examples) | 20 passed, **1 failed** | **21 passed**, 0 failed |
| Feature-gated UI library test | never run | 1 passed |

The engine numbers reconcile exactly: 1,001 + 5 repaired − 41 retired = 965.
The 41 retirements are 11 in `integration_actions`, a net 4 in `doc_host`
(12 tests became 8) and 26 in `sessions`. The harness count is unchanged:
provider peers and wire contracts are untouched (K8).

**Mutation proofs.** Each production mutation was caught by the moved, repaired
or restored keeper, and each file was restored byte-for-byte
([mutations](evidence/mutations.json), [M3](evidence/mutations-M3.json),
[script](evidence/mutations.py.txt)):

| ID | Mutation | Keeper that went red |
| --- | --- | --- |
| Skip | `evaluate_command` stops skipping processed commands | Restored `luna_doc::commands::processed_commands_are_skipped`. Before the restore, nothing failed: 97 doc and 31 engine e2e tests passed. |
| M1 | `save_snapshot_checked` drops the projection lock | Re-homed `approved_commit_blocks_stale_snapshot_saver_until_installed` |
| M2 | `import_transcript` drops the projection lock | Re-homed `projection_blocks_remote_row_until_queue_is_installed` |
| M3 | `is_luna_pending_approval` stops recognising the send kind | Re-homed `generic_answers_cannot_consume_server_owned_luna_approval` |
| M4 | Backup manifests record the schema version minus one | All 5 repaired `backup_restore` round trips |

**Keeper delta (K1).** The rebase found 4 missing keepers before (one a real
loss) and 3 after, with no new missing keeper. The 46 newly absent "retain"
rows are exactly the deliberate retirements above.

**Routing and native evidence:**
- [CI baseline](evidence/ci-baseline.json): run on `afd8455b`.
- [`LunaUITests`](evidence/ios-ui-tests.json): 14 run, 13 passed. The one
  failure reproduces in isolation.
- The new CI steps are verified on the delivered commit; see the hand-off
  below.

**Other checks:**
- Clippy on `luna-engine` and `luna-doc` (all targets): exit 0. Every warning
  in a touched file predates this work; none is on a changed line.
- rustfmt was applied to the touched files; `git diff --check` is clean.
- LUNA Dev was refreshed with this build: 114 s, PID 5154, IPC 59777, `codesign --verify --deep --strict` passes.
- Commands and exit codes: [commands.json](evidence/commands.json). Hashes: [sha256.json](evidence/sha256.json).

## Limitations and hand-off

- **CI verification of the new routes**: on run 38178958258 the routed step
  compiled every fixture example on Linux, passed the feature-gated test and ran
  21 embedded tests (20 passed; the one Linux portability failure is fixed in
  `a19deb8c`). Details in [CI routing](evidence/ci-routing.json).
- **Baseline failures**: follow-up fixes right after CORE-02 resolved every one
  except a timing-flaky iOS layout test, which is flagged as a separate task
  (see the baseline ledger).
- **Coverage gaps made visible.** Fail-closed expiry, run-change and one-shot
  coverage existed only on the retired archive responder; the live send
  responder never had it. The new owner should add it at the send path, from
  a failure-first specification.
- **Backup restore activation** stays dormant until a restore-gates ticket
  owns it. SW-01 owns the two provider observations.
- **Other audit follow-ups**: the per-lane cosmetic and capability-table items
  in the 9 October ledgers stay with their owners. CORE-02 resolved the named
  test-only production paths, the unrouted examples and the lost keeper, not
  every cosmetic row.
