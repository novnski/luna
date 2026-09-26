# Project execution defaults — 26 September 2026

Latest combined verification on `04da0cf3`: **17 affected UI cases passed**,
with full format, focused engine/UI compile and Clippy all exiting 0. Both
Defaults and Commands reject the evidenced stale-engine results. Native app
acceptance remains pending its separately granted desktop handoff. Earlier
failures and exact-source proofs are preserved below.

Branch: `codex/project-execution-defaults`.
Worktree: `/Users/iulian/.codex/worktrees/9f21/luna`.
Base: `d1c71c3a31d6ef7c895adc9084c27293940fce39`.
Preimplementation regression cases: `8ab4632b`.
Source checkpoint: `3650bd0e`; catalog race guard `25d06a81`.
Reviewed backend base `dbb38ee1` adopted without conflicts in `aabb52a1`.
Shared fixture repair `65c3800d` was adopted as `7d4b5f0b` before that merge.

## Contract and scope

Settings → Projects adds persisted provider/model, permissions and checkout
choices per project. Each group can inherit; “Inherit all global defaults”
clears all three groups. Old rows without `executionDefaults` inherit without
rewriting existing tasks. Data uses the existing registry, backup, sync and
DeviceWriteHandle path. The mutation checks owner and the expected previous
value under the registry lock; stale writes fail instead of overwriting newer
settings. Remote writes use the existing targeted relay for this operation.

All desktop new-chat affordances feed the same Pickers/composer resolver.
ComposerDefaults remains the fallback. Provider+model is one group: inherit
uses the global provider and its remembered model; an explicit provider with
no model uses that provider's catalog default. Explicit project choices never
silently fall back when unavailable. The user can pick another model/provider
for that draft or update the project settings. Provider/machine permission
limits and the existing first-Full-access confirmation still apply.

Current checkout retains existing worktree reuse; New worktree emits the
existing queued WorktreeSpec. A non-Git folder cannot use the latter. A project
or device transition clears stale draft selections. Watch updates preserve
explicit draft choices. Project-specific provider/model and permission picks
do not overwrite their global fallback groups.

The coordinator explicitly scoped this first increment to desktop new chats.
Raw CreateChat callers, native forks and provider-created child tasks keep
explicit config/Run contracts. No Plan → Build changes, new provider runtime,
queue authority, task identity or alternate worktree implementation is added.
The Projects container exposes selected_project_id() for the separately owned
ProjectCommandSettings child card; that composition remains coordinator-owned.

## Source comparison

- T3 Code checkout `e5a46d6c5d00b89afba5274a94d42428c8d79763`; safely fetched
  `origin/main` is `2a9832b8019039f812706639c11ccc24ce4c4e05`. Inspected the
  `packages/shared/src/projectSettings.ts` resolution (including the fetched
  ref), `packages/contracts/src/settings.ts` persisted fields and
  `apps/web/src/components/settings/ProjectDefaultsSettings.tsx`.
- BB checkout `9c8112e546f042b923f1a0c080f6b427c2fa60f6`; safely fetched
  `origin/main` is `9c9bae7f36a237c7e1b96de3d4c2186d13967686`. Inspected the
  project route's existing execution-defaults references on the fetched ref.
- Luna reuses its settings widgets, theme, ComposerDefaults, provider catalogs,
  permission checks and checkout plan. Neither reference checkout was changed.

## Repeatable verification

Requires a coordinator-granted build slot:

```sh
scripts/qa-project-defaults.sh
```

The runner saves logs and `persistence.json` under
a timestamped directory beneath `target/qa/project-defaults`, preserves exact
commands/source/exit codes and stops on the shared guard. An explicit
`LUNA_DEFAULTS_QA_OUTPUT` selects a named batch directory.
The first-written cases are in `project-defaults-cases.md`, the engine RPC /
registry restart integration target and the GPUI picker integration module.
No provider invocation is required for these cases.

Native QA requires a separately granted app slot:

```sh
LUNA_DEV_APP_NAME='LUNA QA Defaults' \
LUNA_DEV_BUNDLE_ID=com.novnski.luna.qa.defaults \
LUNA_DEV_IPC_PORT=59788 \
LUNA_DEV_DATA_DIR="$PWD/target/luna-qa-data/defaults" \
CARGO_INCREMENTAL=0 scripts/run-macos-dev.sh --refresh
node scripts/qa-project-defaults-live.mjs seed
node scripts/qa-project-defaults-live.mjs read
```

The live helper only accepts QA port 59788 and never sends prompts. It creates
or reuses two Git fixtures within this checkout's QA artifact directory and
saves authoritative project/task readback. `clear` explicitly restores both QA
projects to inherit. Keep fixtures while the packaged QA app refers to them.

## Verification status

The first granted batch ran with `CARGO_INCREMENTAL=0` and the standard wrapper
on `aabb52a1` (only the logging script differed). Initial failures are retained.

| Check | Result | Evidence |
| --- | --- | --- |
| Engine RPC/registry/restart integration | 2 passed, 0 failed; compile 51.28s, tests 0.05s | `target/qa/project-defaults/batch-1/engine.log`, `persistence.json` |
| Picker GPUI integration compilation | Exit 101: new Projects category missing from existing sidebar icon match | `target/qa/project-defaults/batch-1/picker.log` |
| Picker retry after one-line existing-icon fix | Exit 75 before compilation: aggregate reservation blocked | `target/qa/project-defaults/batch-2/picker.log` |
| Source formatting, shell/Node syntax, diff whitespace | Passed static checks | Source checkpoint / git diff checks |
| Corrected picker GPUI integration | 3 passed, 0 failed; compile 29.91s, tests 0.03s (34s wall) | `target/qa/project-defaults/batch-3/picker.log` |
| Full workspace format check | Exit 0, 5s wall | `target/qa/project-defaults/batch-3/format.log` |
| Focused engine/UI compile | Exit 0, 48s wall | `target/qa/project-defaults/batch-3/compile.log` |
| Focused engine/UI Clippy | Exit 0, 35s wall; existing warnings, none in the new defaults modules | `target/qa/project-defaults/batch-3/lint.log` |
| Native app/UI/IPC/signature | Pending separately granted slot | No native pass claimed |

Engine coverage confirms missing-field migration through serialized registry
state, owner/missing-project/malformed-value rejection, stale expected-value
rejection, restart persistence, explicit inherit clearing and unchanged existing
task rows. The corrected GPUI cases verify A → B → A transitions, reconstructed
picker state, inherit clearing, retained explicit draft choices across watch
updates, global fallback preservation, existing task config authority,
provider-default model semantics, unavailable model retention/blocking and
first-Full-access confirmation/recovery. Native visual behavior remains pending.

The second admission observed 156,642,626,920 bytes (145.89 GiB) in aggregate;
adding the default 25 GiB reservation would exceed the 170 GiB operating limit.
No reservation override, retry loop, cache cleanup or native launch followed.
The build slot was released and the coordinator notified. This checkout's
`target/debug` measured 7.1 GiB; it is preserved for the pending verification.

After coordinator-managed normal 20 GiB cache collection, batch 3 was explicitly
regranted and passed on `6d637af9` without lowering the 25 GiB reservation or
changing cache policy. `commands.txt`, `source.txt`, `exits.tsv`, `result.txt`
and before/after accounting are retained with that batch. Final aggregate was
154,439,057,096 bytes (143.83 GiB); this target/debug was 9.0 GiB and available
disk space 393 GiB. The compiler slot was released after the checks.

## Combined Projects verification

Commands source and the shared Projects card were adopted at `cca464e8`.
Its 28-line composition preserves the existing defaults controls and keys the
child card by project, owning device and engine identity. Batch 4 verified
Defaults engine 2/2 and Commands engine 5/5, then preserved a UI compile failure:
`gpui::Timer` was unavailable in the Commands panel. Commands owner corrected
that UI-only call using the existing executor timer in `3e62cf03`.

Batch 5 verified the exact clean runtime source
`3e62cf0379209a2c9795aae372a8c491c7a1e234`:

| Check | Result | Wall time |
| --- | --- | --- |
| Picker GPUI integration | 3 passed, 0 failed | 40s |
| Full workspace format | Exit 0 | 5s |
| Focused engine/UI check | Exit 0 | 41s |
| Focused engine/UI Clippy | Exit 0 | 53s |

The passing combined engine 2+5 cases at `cca464e8` were not needlessly rerun
for the UI-only timer correction. The Commands module still has a nonblocking
unused `AnyElement` import warning; it was relayed to its owner. No new defaults
module warning appeared.

Both combined batches used the coordinator-approved temporary
`LUNA_BUILD_RESERVE_GIB=10`, `CARGO_INCREMENTAL=0`, `scripts/cargo.sh`, and
`--locked`. This is a scoped warm-check reservation, not a policy change;
the 170 GiB ceiling and 10 GiB growth guard remained enforced. Batch 5 grew
aggregate usage from 153.688 to 154.056 GiB, with 389 GiB disk space available.
No targets, apps, data or evidence were deleted. Cargo was released afterward.

Evidence is retained under `target/qa/project-defaults/batch-4-combined` and
`batch-5-combined`, including exact source, command lines, exits, logs and
before/after budget JSON. Native/desktop QA remains separately gated.

### Newly identified source concern, not covered by the passing cases

Commands owner found that a retained terminal command panel can accept an old
engine's response after same-chat engine replacement. Defaults source review
also found its save callback matches only `space.id`, and model requests do not
explicitly fence engine generation. An old result could therefore affect a
new backend with the same project identity. These are source-derived findings,
not runtime reproductions. The coordinator and Commands owner were notified;
production source remains `3e62cf03` while scoped failure-first correction is
coordinated. The ten focused passing cases do not establish this lifecycle path.

### Genuine Defaults engine-replacement baseline

The coordinator granted the Defaults-only baseline on exact test source
`37afc8c813521f93f184d070f326a1821833ed6c`, whose production runtime remains
`3e62cf03`. The only dirty file was the three-line clarification of stale
busy/loading retirement in `project-defaults-cases.md`; its exact patch was
recorded without reverting or stashing it.

The command compiled successfully and ran eight integration cases: **0 passed,
8 failed**, exit 101, 54s wall. Every failure reached the strict before/after
state assertion after the actual retained ProjectsPage callback completed;
there were no compiler errors, fixture timeouts or setup panics. The traces
confirm real engine-A replies after actual engine-B attachment, with generation
advancing from 1 to 2 in both same-device and different-device variants:

- Save success replaced B's project model with `late-engine-A-save`.
- Save failure displayed A's error in B's retained settings page.
- Model success replaced B's catalog with `late-engine-A-model`.
- Model failure displayed A's catalog error in B's page.

The explicitly selected B draft remained unchanged in all eight cases. This
narrows the confirmed defect to retained page/callback state and project-row
projection. Tests use the shared Commands-owned held-RPC fixture, the real
in-memory RPC transport and the actual AppState attachment path; they do not
invoke synthetic completion callbacks.

All eight before/after/transport traces, a compact `summary.json`, exact source,
command, dirty-doc patch, exit and pre/post budget are preserved under
`target/qa/project-defaults/epoch-baseline-1`. The sole Cargo slot was released
at completion. No production identity fence has been applied at this baseline.

### Defaults correction prepared after the failing baseline

The Defaults-owned production correction is limited to its settings module.
Each save/catalog request captures project ID, project owner, engine generation
and engine device. The retained page retires waits and stale busy/catalog/error
indicators when that identity changes. Completion callbacks also compare the
live identity before publishing success, errors, catalogs or project-row updates,
covering replies that become runnable before the state observer executes.
Model replies additionally retain the existing per-request generation/provider
checks. Project selection, composer drafts and the shared Commands card contract
are unchanged. Cancellation of the UI wait does not undo a write already
committed by the old engine.

This source correction was made only after the eight authentic failures were
recorded. Test assertions and the shared fixture were not changed. Direct
rustfmt and diff checks passed; no fixed-run pass is claimed before the next
granted verification batch.

The separate Commands baseline on clean `97804ba0` compiled and ran six cases,
with an observed **0 passed / 6 failed**, not the hypothesized 5+1. Five stale
status/error/output/busy assertions failed; the same-engine control reached its
final dropped-view polling assertion and also failed (3 calls versus 2).
That sixth failure is awaiting ownership/timing diagnosis by its test owner;
it is not classified here as an additional confirmed stale-engine defect.
All six JSON artifacts and exact source/log/exit/budget evidence are retained
under `target/qa/project-defaults/commands-epoch-baseline-1`. Cargo was released,
and no Commands production code was changed by the Defaults worker.


### Fixed-run fixture scheduling repair (no pass claimed)

The first fixed run on `b133f357` compiled and emitted all eight traces with
unchanged B state and canceled A requests, but reported **0 passed / 8 failed**
at GPUI's end-of-test deterministic-scheduler check. The failure was a Tokio
worker-thread wake into the test foreground executor, not the stale-state
assertion. The exact Commands control was not reached because the batch stopped.
Evidence is retained under `target/qa/project-defaults/epoch-fixed-1`.

The narrow fixture correction changes only the Defaults test runtime from a
multi-thread Tokio runtime to `new_current_thread`, matching the Commands
fixture and the shared pump's `runtime.block_on(yield_now())` topology. Real
memory RPC, actual engine attachment, held reply/cancellation boundaries,
before/after snapshots and every assertion remain unchanged. Commands helper
owner confirmed this topology; shared helper and production code are unchanged.
This is a source diagnosis, not a passing run. Prior baseline traces still show
actual late A mutations; their early assertion panics did not reach the final
scheduler check. The runtime topology difference is retained explicitly in the
baseline/fixed evidence rather than treating the failed fixed run as a pass.


### Corrected fixed run passes

On exact clean `8611168911eae2ccf3aab71bccaf57542b409962`, the coordinator's
bounded rerun passed the Defaults held-RPC cases **8/8** (43.74s compile, 0.03s
tests, 52s wall) and the exact corrected Commands same-engine control **1/1**
(1.39s warm compile, 0.01s test, 9s wall). Both commands exited 0; no scheduler
failure was suppressed and no assertion was changed.

Every Defaults trace shows B's project/page/catalog/draft state unchanged after
A's request is canceled. The Commands control retains normal load/execute/Output
behavior and proves `parentReleased=true`, `commandsReleased=true`, with status
calls unchanged at 2 before and after actual disposal. This confirms the prior
control issue was its disposal boundary; the five Commands stale-runtime cases
still await their owner's production fence and subsequent verification.

Evidence: `target/qa/project-defaults/epoch-fixed-2` contains logs, eight Defaults
traces, Commands normal/drop traces, `summary.json`, exact source/commands/exits
and budget snapshots. Aggregate usage was 149.189 → 149.423 GiB with the
coordinator-approved temporary 10 GiB reservation and unchanged 170 GiB ceiling.
Cargo was released at completion; the passing control evidence was sent directly
to the Commands owner under the coordinator's conditional source-only approval.
No native app or extra test targets ran. The repeatable Defaults runner now
includes its eight held-RPC cases and retains their JSON in `engine-epoch`.


### Both Projects fences verified together

The Commands-owned production fence `40b0720c` was source-reviewed and merged
with the verified Defaults guard into clean `04da0cf3b4c9b38f5bfb2d35b7830ef321da7481`.
It pins load/execute results and automatic/manual Output events to engine
scope, retires stale cached/busy/request state, and makes the parent terminal
listener reject stale scope before selecting or looking up output. It retains
the existing one-second poll cadence and does not cancel accepted backend jobs.
No assertion or shared fixture was changed during this production correction.

The explicitly granted combined batch passed:

| Check | Result | Wall time |
| --- | --- | --- |
| Both engine-epoch suites | 14 passed, 0 failed | 51s |
| Original picker defaults regressions | 3 passed, 0 failed | 8s |
| Full workspace format | Exit 0 | 6s |
| Focused engine/UI compile | Exit 0 | 21s |
| Focused engine/UI Clippy | Exit 0 | 22s |

All eight Defaults state/draft snapshots remain equal. Commands traces prove
that stale Output produces no replacement-engine terminal lookup, B's status
can load after retiring A, and normal same-engine execution/disposal remains
usable. No warning was attributed to either new Projects settings/commands
module in this Clippy run; existing repository warnings remain in the log.
Previously passing backend suites were retained, not rerun unnecessarily.

Evidence: `target/qa/project-defaults/combined-final-1`, including exact source,
commands, exits, logs, 15 held-RPC JSON artifacts, compact `summary.json`, and
actual before/after budget. Aggregate usage was 149.409 → 149.672 GiB under the
coordinator-approved temporary 10 GiB reservation and unchanged 170 GiB ceiling.
This target/debug measured 9.1 GiB; free disk was about 390 GiB. Cargo was released
promptly and the Commands owner/coordinator received the results. No native
app was packaged or launched in this grant.


### Rendered Commands admission baseline (separate from the 17 passes)

The accepted `04da0cf3` result-isolation/picker proof is unchanged. Subsequent
source review identified an outbound rendered-input question, which Commands
owned and specified before an admission fix. Its first five-case run on
`896106a1` failed entirely on a root-view lease fixture panic before traces. A
test-only raw-window correction was adopted at `0ef61afd`; that second run had
one fresh-control pass and four failed retained-frame premises. Neither run
established a behavioral admission defect; both complete artifact sets remain.

Commands then modeled the native pre-redraw boundary inside one raw Window/App
update, separating delivery-time counters from GPUI test-support's eager
post-update repaint. On exact clean `80bd8e08`, the granted baseline-3 compiled
and ran five cases: **1 passed / 4 failed**, exit 101, 52s wall. Every frame/bounds
premise held. Fresh input passed; stale Run, Retry and Stop each dispatched once
to engine B; an enabled old button admitted a target request while live busy.
For each stale case, AppState generation was 2 while cached/painted origin was
1; render counters remained 2 → 2 during input, then became 3 only after the
outer update. These are genuine behavioral failures, independently accepted by
the coordinator, not the earlier fixture failures.

Evidence is under `target/qa/project-defaults/button-admission-baseline-3`, with
five raw traces, `summary.json`, exact source/command/exit/log and actual budget.
The default 25 GiB reservation was used; aggregate was 140.623 → 140.857 GiB.
Cargo was explicitly released. Commands owns the admission fix; Defaults may now
write its separately specified six-case suite using this validated boundary,
before any Defaults admission guard. No native interaction occurred.


### Commands admission fix and retained regressions pass

The Commands-owned 13-line runtime admission correction `a679a411` was reviewed
and adopted in exact clean `59fbe099569b3e5d3ed371cbe8b072bac2e62228`. Rendered
Run/Retry/Stop callbacks capture origin scope; execution also rejects live busy
re-entry without replacing its existing waiter. Output/result fences, PTYs,
poll cadence, layout and theme are unchanged.

The granted affected batch passed **19/19** engine-epoch/admission cases (prior
14 plus five rendered-button cases), **3/3** original picker cases, full format,
focused `luna-ui --lib` check and Clippy. Wall times were 49s, 7s, 6s, 21s and 26s;
all exits were 0. Fixed traces show zero B dispatches for stale Run/Retry/Stop
and zero target build requests while busy, with unchanged delivery frame counts
and a passing fresh control.

Evidence: `target/qa/project-defaults/commands-admission-fixed-1`, including
source/commands/exits/logs, frame/RPC JSON, `admission-summary.json` and actual
budget. Default reservation was 25 GiB. Cargo was explicitly released; no new
Defaults admission cases, backend repeats, native package or source edits ran
during this batch. Defaults' separate six-case admission reproduction is now
source-only and awaits its own grant; these 22 passes do not claim that coverage.
