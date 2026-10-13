# ZI-01: isolated and bounded Latest turn snapshots

Baseline: main `3c959fb3`. Shared queue owns status. Specification:
[implementation § ZI-01](../implementation.md#zi-01--isolate-and-bound-turn-snapshots),
[research § ZI-01](../research.md#zi-01-the-snapshot-index-is-isolated-but-its-objects-are-not).

## Where the baseline touches the checkout

Proved from `crates/engine/src/diff_sync.rs` at the baseline:

| Call | Effect on the checkout |
| --- | --- |
| `snapshot_tree` → `git add -A --ignore-errors .` under a temp `GIT_INDEX_FILE` | Writes a loose blob for every hashed tracked/untracked file into `$GIT_COMMON_DIR/objects`. The temp index protects staging, not objects. |
| `snapshot_tree` → `git write-tree` | Writes every tree object into the same database. Nothing references them. |
| `note_turn_start` (every dispatch, steer and provider-confirmed boundary) | One `snapshot_tree` per turn, in the background, with `output()` (no timeout, no kill on drop, no untracked budget). A failure or non-checkout cwd keeps the previous turn's `turn_trees` entry; a slower old capture can overwrite a newer one. |
| `capture_turn_diff` (each `GetCheckoutDiff` turn read; twice per `GetCheckoutFileDiffText`) | Another `snapshot_tree` per read, then tree diffs that only work because the objects were written into the checkout. |
| `read_diff_file_text_at` turn mode | `git cat-file blob <turn tree>:<path>` from the checkout database. |

`git_checkpoints.rs` (engine/git) deliberately stores reachable private refs
`refs/luna/checkpoints/*` for File restore. That durable feature is out of
scope; the E2E accounts for its reachable objects instead of treating them as
snapshot pollution.

## Failure contract before code

| # | Credible failure | Contract | Owning proof |
| --- | --- | --- | --- |
| F1 | Snapshot blobs/trees land in the checkout database | Every snapshot command runs with an absolute private `GIT_OBJECT_DIRECTORY`; the checkout's real object directory (resolved by `git rev-parse --path-format=absolute --git-path objects`, so linked worktrees use the common dir) is only an alternate | Byte manifests of the common `objects/` tree before/after |
| F2 | Reads look for private objects in the checkout alone | Capture, tree diff and `cat-file` share one context: comparison store → base store → checkout objects | Oracle patch and old/new text equality |
| F3 | Real index, staging, refs, hooks or config change | Private `GIT_INDEX_FILE` inside the store; `GIT_DIR`/`GIT_WORK_TREE`/`GIT_COMMON_DIR`/object env removed; `core.hooksPath` inside the store, `core.fsmonitor`, `core.splitIndex`, `core.untrackedCache` off; listing commands are read-only against the real index | Index bytes hash, `ls-files -s`, `for-each-ref`, `status` before/after |
| F4 | Untracked build tree/dataset hashes unbounded bytes | Untracked files > 8 MiB are omitted; a wholly untracked directory over 10,000 files / 256 MiB is omitted as one unit; then the largest units go until the rest fits; listing capped at 8 MiB; > 2,048 omitted units or a capped listing makes the scope unavailable | Large-directory E2E |
| F5 | Omitting a path only on one side fabricates mass additions/deletions; an omission looks complete | The comparison starts from the base tree and excludes the union of both sides' omissions; any omission sets `truncated` (existing "Partial snapshot" chip) and appends `# Luna turn snapshot omitted N path(s) over the snapshot budget` | Directory crossing the threshold mid-turn |
| F6 | Tracked change silently missing | Tracked files are never budgeted. Tracked-but-ignored files (measured in 3 of 5 local repos) are added explicitly. Over-time → unavailable, never partial | Force-added ignored file appears in the oracle-equal patch |
| F7 | Hung Git child, clean filter or textconv survives cancel/timeout | Every snapshot Git child runs in its own process group, `kill_on_drop`, and is `SIGKILL`ed as a group while still unreaped; the whole capture (permit wait included) runs under a 60 s budget | Gated clean filter: pid exits after supersede and after shutdown; later capture works |
| F8 | Stale previous-turn base answers for the new turn | `note_turn_start` synchronously replaces the chat's slot with a pending generation and cancels the old capture. Only the matching generation publishes. Failure, non-checkout cwd or another checkout → explicit "Latest turn is unavailable: …"; never the prior turn | Overlapping starts; failure after a good turn |
| F9 | Store removed while a reader needs it / never removed | Store lifetime is an `Arc` shared by every clone of the `TurnSnapshot`; a reader holds its clone for the whole RPC. Slots are released on replacement, on shutdown, after the chat is absent/archived for the orphan grace, or when more than 32 ready bases are retained (oldest first, reported unavailable) | Held reader survives replacement; store count returns to one |
| F10 | Crash sweep removes another engine/profile's live store | Stores live in `<device data root>/turn-snapshots/<pid-start-run>/<uuid>`. Only `CheckoutDiffSync::start` sweeps, during engine assembly under the device-root `InstanceLock`, and only sibling run directories of this data root | Stale run collected; another data root untouched |
| F11 | Paths with spaces/`:`/quotes, linked worktrees, unborn HEAD break lookup | Alternate entries are C-quoted when they contain the list separator or start with `"`; absolute paths only; unborn HEAD uses the same tree path | Table over ordinary/unborn/linked worktree with `:` and spaces in data and checkout paths |
| F12 | Concurrent captures pile up | At most 2 snapshot captures per engine; waiting counts against the budget | Code review; covered indirectly by the hung-filter E2E |

Authoring gate: the Latest turn contract is owned by one engine-boundary suite
(`crates/engine/tests/turn_snapshot_e2e.rs`) that drives the public turn hook
and RPC and an independent Git oracle in a separate bare object database. The
older direct-API test in `m5_repos_diffs_terminals.rs` is folded into it. No
test-only production seam, environment knob or unit test is added.

## Measured budgets

Read-only measurements on 2026-10-13 (macOS, warm cache, Apple Git 2.54):

| Repo | Tracked files | Tracked bytes | `git hash-object` pass | Untracked files | Tracked-ignored |
| --- | --- | --- | --- | --- | --- |
| luna | 3,336 | 118.6 MiB | 0.70 s | 1 | 0 |
| t3code | 27,814 | 307.3 MiB | 4.12 s | 1 | 35 |
| opencode | 6,628 | 118.5 MiB | — | 0 | 5 |
| codex | 8,815 | 83.0 MiB | — | 0 | 3 |
| zeron | 2,727 | 70.8 MiB | — | 0 | 0 |

The largest measured tracked file is 4.3 MiB, so the 8 MiB untracked per-file
cap keeps ordinary assets; 10,000 files / 256 MiB is two orders above any
measured untracked set. The 60 s capture budget is ~15× the largest warm hash
pass (upstream's 120 s was not adopted unmeasured). These are planning bounds;
the E2E artifact records actual cold/warm durations for regression limits.

## Delivery

- `crates/engine/src/diff_sync/turn_snapshot.rs` (new) owns the snapshot
  handle (`TurnSnapshot`: canonical root, checkout id, tree, generation,
  omitted units, `Arc` store) and the per-engine `SnapshotRuntime` (store run
  directory under the device data root, 2 capture permits, stale-run sweep).
  Each capture: resolve the real object dir through Git, create the store,
  triage untracked units, optionally `read-tree` the base, `add -A` with
  NUL-separated exclusion pathspecs on stdin, `update-index` tracked-ignored
  paths, `write-tree`, delete the private index. Every Git child is
  env-cleaned, config-hardened, process-grouped, `kill_on_drop`, bounded in
  stdout/stderr and wrapped in the 60 s budget.
- `crates/engine/src/diff_sync.rs`: chat slots (`Pending`/`Ready`/
  `Unavailable` with a generation); `note_turn_start` replaces synchronously
  and cancels the superseded capture; `latest_turn_base` waits for a pending
  capture and checks the checkout identity; reconcile releases bases of
  archived/absent chats after the existing orphan grace; shutdown releases all;
  `capture_turn_diff` layers the comparison on the base and reads only through
  the snapshots' context, marking omissions partial. Watch-path `capture_git`
  children now die with a dropped capture (`kill_on_drop`).
- Shared hunks: `rpc.rs` turn arms of `GetCheckoutDiff`/`GetCheckoutFileDiffText`
  hold the base for the whole read; `repos.rs` adds `Repos::data_dir`;
  `lib.rs` drops the now-internal `snapshot_tree` export.
- UI and protocol are unchanged: partial reuses `truncated` ("Partial
  snapshot") plus a patch comment line the parser skips, so even an all-omitted
  result renders as partial rather than clean; unavailable reasons arrive as
  the existing scoped warning text. "no turn recorded" is kept verbatim.

Stores are device-scoped (`<data root>/turn-snapshots/<run>`), not
profile-scoped: the `InstanceLock` that proves a sibling run is dead is held on
the device root, and a crash in one profile is then collected by the next
engine start in any profile. Chat snapshots never outlive their engine.

## Criteria mapping

| Criterion | Evidence |
| --- | --- |
| contract | This file: baseline proof, F1–F12, measured budgets, store/reader/sweep ownership. |
| delivery | Code above; tracked state/index/refs intact and stale generations cannot publish (F3, F8). |
| verification | `turn_snapshot_e2e` (8 cases, below) with oracle equality and object/index/ref manifests; artifacts via `LUNA_ZI01_ARTIFACT_DIR`. |
| handoff | Orchestrator: commands, exit codes, artifact hashes, app refresh. |

| E2E | Failure modes |
| --- | --- |
| `ordinary_checkout_…`, `unborn_head_…`, `linked_worktree_…` (real dispatched mock turn) | F1, F2, F3, F6, F11; pre-turn staged/unstaged/untracked state never reappears; only checkpoint-ref-reachable objects appear after a turn, and Latest turn reads change no object/index/ref byte |
| `turn_hook_writes_only_private_stores_and_collects_only_stale_runs` | F1–F3 exactly (no checkpoints): blob only a private store holds is read back; F10 crashed run collected, live store and foreign data root kept |
| `untracked_budget_omits_consistently_and_reports_partial` | F4, F5: oversized file, 10,001-file directory, directory growing past and another shrinking under budget; oracle with the same omissions; marker counts 2 then 3 |
| `turn_starts_never_answer_with_a_prior_base` (Unix) | F7, F8: non-checkout cwd, failing required filter, superseded hung capture (pid exits) never publish |
| `hung_capture_dies_with_shutdown_and_restart_recovers` (Unix) | F7: hung filter pid exits on shutdown, store removed, restart captures |
| `reader_keeps_a_replaced_base_until_it_finishes` (Unix) | F9: held base survives replacement, released after the reader, one store retained |

The suite uses only APIs that also exist at the baseline, so it can be run
there to confirm each regression (not run by the author). Expected baseline
failures: snapshot objects in the checkout (manifest/unexplained-object
asserts; Ordinary also misses the tracked-ignored change in the oracle patch),
`truncated == false` for omitted units, `expect_err` returning the previous
base, the hung filter pid never exiting, and no private store or stale-run
sweep to observe.

## Commands for the orchestrator

```sh
scripts/cargo.sh check --locked -p luna-engine --all-targets
art="target/qa/zi-01/$(git rev-parse --short HEAD)"; mkdir -p "$art"
LUNA_ZI01_ARTIFACT_DIR="$PWD/$art" CARGO_INCREMENTAL=0 \
  scripts/cargo.sh test --locked -p luna-engine --test turn_snapshot_e2e 2>&1 | tee "$art/turn_snapshot_e2e.log"
scripts/cargo.sh test --locked -p luna-engine --test m5_repos_diffs_terminals
scripts/cargo.sh test --locked -p luna-engine --test diff_sync_churn
scripts/cargo.sh test --locked -p luna-engine --lib -- diff_sync git_checkpoints
scripts/cargo.sh clippy --locked -p luna-engine --all-targets -- -D warnings
shasum -a 256 "$art"/*.json
```

Expected: 8 passed in `turn_snapshot_e2e` on macOS; the existing suites pass
unchanged (the folded `turn_diff_captures_only_changes_since_snapshot` is gone).

Native QA (LUNA Dev, disposable repo, read-only elsewhere): record
`git count-objects -v`, the index hash and `git for-each-ref` of the repo; run a
turn; open Changes → Latest turn (complete); add an untracked directory of
10,001 files and a > 8 MiB file and run another turn (expect "Partial
snapshot"); start a turn from a non-Git folder in the same chat (expect "Latest
turn is unavailable: the turn started outside a Git checkout"); re-read the
repo manifests (unchanged except `refs/luna/checkpoints/*` and objects they
reach) and confirm `<LUNA Dev data>/turn-snapshots/<run>/` holds one store per
chat and is emptied after quitting. A crash-collection proof needs a scoped QA
app identity (QA guide) that may be killed; never the primary app.

## Limitations

- The 60 s timer itself is not waited out; it drops the same future (and kills
  the same process groups) that supersede and shutdown exercise. The 256 MiB
  byte budget shares the file-count path and is not exercised with real bytes.
- Git still refreshes mtimes of checkout objects a snapshot finds there. A base
  tree may reference blobs that exist only in the checkout (committed, staged,
  or written by File restore checkpoints); pruning the checkout during a turn
  makes that turn's read fail explicitly, never silently wrong.
- Release of archived/deleted chats waits the existing 120 s orphan grace and is
  code-reviewed, not E2E-timed. Retained bases are capped at 32 per engine.
- A file that becomes ignored mid-turn keeps its base entry (layered capture)
  instead of showing as deleted. A filter that daemonizes out of Git's process
  group is outside the group kill; a hung filesystem can pin one blocking walk
  thread until its next entry, though the capture itself returns at the budget.
- Requires Git ≥ 2.31 (already required by checkout identity). Windows quoting
  (`;`) is implemented but not built or exercised here.
- A crash-left store under the device root counts as unknown data for an
  isolated backup-restore target until the next engine start sweeps it.
