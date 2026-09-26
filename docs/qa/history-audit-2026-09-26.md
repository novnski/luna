# Usage, history and preferences audit — 26 September 2026

## Scope

Author-focused review from `7b141044c10650cc2ec476128763384cf5084cfe`, on
`codex/audit-history-preferences`. The seven fixes in
`backend-audit-2026-09-26.md` are the baseline. This pass follows usage scanning,
cache refresh and migration, snapshot aggregation, activity indexing/search,
and General preference persistence. No visual styling or UI files change.

## Failure modes recorded before implementation

- The UI merges even a single environment snapshot. The merge clears the
  scanner's pricing warnings, hiding missing or stale pricing evidence.
- A newer complete environment can erase an older environment's
  `summary.partial` state when its sources themselves are ready.
- A version-2 cache is discarded during the version-3 parser upgrade. If the
  provider deleted a rollout after it was cached, the historical totals vanish.
- Preserving a compatible old cache must still force existing transcripts to
  reparse. Old records must not override fresh parser output or double count
  the same rollout after it moves to `archived_sessions`.
- Unknown future cache versions must not be accepted as compatible evidence.
- If the provider removes the entire transcript directory, an early return
  bypasses otherwise valid cached history. A separate integration regression
  was recorded before correcting that fallback.
- Infrastructure commit `d1c71c3a` gives QA apps a separate data directory.
  Under a Codex-managed worktree, Usage rejected that directory as part of
  the provider home and disabled both pricing and scan caches. A subprocess
  regression with an isolated HOME covers the permitted path and rejects
  arbitrary provider-home descendants, nested paths, escaping symlinks, missing
  worktree registrations and copied registrations belonging to another checkout.

The integration regressions in `crates/engine/tests/usage_history.rs` were
written before implementation. They use actual JSONL files and disk caches,
copy caches into a second data directory to simulate a cold process, and call
the same public scanner/merge path as the Usage UI. The original test patch is
retained in `target/qa/history-audit-2026-09-26/`.

## Corrections

Snapshot aggregation retains distinct input warnings and incomplete-summary
state while preserving physical-source deduplication. The original warning
loss traces to `noviulian` commit `1d2c5ebe8`.

Cache loading recognizes the compatible version-2 record schema and marks its
records as legacy evidence. Available files always reparse. Unavailable
records remain visible with a partial status and a warning; that provenance
survives disk persistence and another cold load. A current record matching the
same provider, session, timestamp, model and token counts supersedes its
legacy copy, preventing archive-move double counting. Unknown cache versions
retain the existing rejection behavior. This handles the migration introduced
by the baseline's parser-version bump without undoing its fixes.

A missing transcript directory now still contributes in-window cached records,
marked partial because the original source cannot be checked. A missing
directory without retained records remains missing. The surviving provider
home is canonicalized before resolving either transcript directory, preserving
cache identity after a child disappears on macOS (`/var` versus `/private/var`).
Both removed `sessions` and removed `archived_sessions` are covered.

The existing narrow worktree-data exception now also permits one app identity
directly below `target/luna-qa-data`. It still checks the canonical path and
requires a `.codex/worktrees` ancestor plus matching reciprocal Git worktree
registration; arbitrary provider-home paths remain excluded. This supports the
coordinator-assigned QA app without touching the user's provider data.

## Other reviewed paths and limits

- General's automatic-retry setting is flushed immediately and read by the
  engine at recovery decision and dispatch. The suspected wiring gap was
  rejected. SettingsStore cancels stale debounce tasks; Shell preserves
  independently owned preference fields when publishing geometry changes.
- Activity search uses stable timestamp/sequence/event-id cursors, project
  scoping and indexed filtering. This pass found no confirmed new defect in
  those paths. Existing engine/RPC history integration coverage is included.
- A read-only metadata sample of 120 recent Codex rollouts found 14 forks;
  sampled first usage events did not establish the suspected fork-copy timing
  defect. One available Claude transcript had one usage row, insufficient to
  validate the changing-duplicate-usage hypothesis. Neither parser behavior
  was changed on speculation. No prompts, auth data or tool output were saved.
- The account-selection race around a slow usage scan was passed to the
  provider/account worker, which owns the RPC selection fence.
- Legacy cached totals cannot recover calls the old parser never recorded.
  Their incomplete status is intentional. A cache already discarded by an
  earlier run cannot be recovered without a surviving copy.

## Repeatable verification

```sh
scripts/qa-history-audit.sh
scripts/qa-history-audit.sh --live-readback
```

The script uses `scripts/cargo.sh`, `--locked`, and `CARGO_INCREMENTAL=0`, keeps
the worktree's target, and saves command files, logs, exit codes, timings and
cache reports in a unique `target/qa/history-audit.*` directory. Live readback
reads the selected provider transcripts and public pricing endpoint; it does
not create provider sessions. Native app verification must use the reserved
`LUNA QA History` identity, `com.novnski.luna.qa.history`, and IPC port `59784`.

## Focused verification results

The original `d1c71c3a` implementation compiled successfully against the new
regressions in 58 seconds. It produced **5 runtime failures and 6 passes**:

| Defect | Original result | Corrected result |
| --- | --- | --- |
| Pricing warning erased by an environment merge | Empty warnings | Scanner warning retained once; duplicate-source totals remain 20 tokens |
| Older partial summary erased | Complete merged summary | Merged summary remains partial and retains its warning |
| Version-2 cache discarded after source deletion | 0 instead of 20 tokens | 20 tokens with explicit legacy warning, including a second cold disk load |
| Removed transcript directory bypasses cache | 0 instead of 20 tokens | 20 tokens, partial, for both directories and both cache versions |
| Worktree QA data rejected as a provider-home write | No cache file created | Cache persists only in the approved, registered worktree location |

The first fixed run exposed the macOS path-alias issue described above. That
failure is retained separately; its isolated rerun passes all four
directory/cache-version combinations. No failed checks were discarded.

Final focused result: **35 passed, zero failed, two explicit live opt-ins
ignored**. This includes 11 usage-history integrations, 2 engine/RPC history
integrations, 19 existing usage tests and 3 existing merge tests. Locked
compilation, formatting, diff whitespace, Bash syntax, ShellCheck and Clippy
passed. Existing dependency/code warnings remain in the retained logs.

Artifacts under `target/qa/history-audit-2026-09-26/`:

- `reproductions/before.log`, `before-result.txt`, original command and saved
  source/test files identify the compiled baseline and its runtime failures.
- `fixed/history.log` retains the intermediate directory failure;
  `fixed/directory-rerun.log` retains its successful isolated correction.
- `final/results.tsv`, command/log pairs and mbx reports retain all final
  check results. `test-counts.json` summarizes the test counts.
- `storage-focused-final.json` records the aggregate guard accounting. The
  worktree's debug target is approximately 3.1 GiB; no release target or
  additional build directory was created. Disposable fixtures cleaned up.

The focused build slot has been released. Native QA app and read-only live
endpoint verification remain queued separately; no app readiness is claimed
by these focused results.
