# Project commands and worktree setup — 26 September 2026

Backend acceptance is complete. Combined Projects UI is source-composed and still
requires UI compilation, native interaction, app refresh and identity readback.
No hosted commands, provider runs or primary-app mutations were used in this QA.

## Source and implementation

Worktree `/Users/iulian/.codex/worktrees/0ace/luna`, branch
`codex/project-commands-setup`. Reviewed runtime base `dbb38ee1` is merged.
Backend checkpoint `eaf304f756f95d35795484fd51c4669c337222f3` passed the checks below.
The separate schema13 prerequisite is `fdefdfba`; backup fixture alignment follows
in `881abae2`. Defaults dependency `acf674eb712efad79ba2a871f7a7174fe230dd64` is merged
as `8ce11c6d`. Its only merge conflict was the adjacent proto module/reexports;
both commands and defaults exports were retained.

The command editor is composed beneath defaults in the existing Projects route.
The page owns its route, selection, title and scrolling. The command child is
recreated when project ID, owning device, engine generation or engine device
changes. Terminal controls reuse existing PTY history and tab attachment; Preview
uses the existing browser action. Theme controls/tokens are reused.

Configuration and receipts use schema13 of the existing main database, with
DeviceWrite admission, explicit local ownership, project/task cleanup and
consistent backup snapshots. Setup receipts precede checkout publication; cwd is
flushed before the PTY handshake releases command execution. Captured action
snapshots, generation identity and attempt fencing prevent stale release/replay.
Stale editor saves use required expectedActions under the existing configure gate.
See `docs/reference/project-commands.md` for execution and restart semantics.

## Verification

All commands used `scripts/cargo.sh`, `--locked`, `CARGO_INCREMENTAL=0` and the
default25GiB reservation for the final backend batch. Evidence directory:
`/Users/iulian/.codex/worktrees/0ace/luna/target/qa/commands-backend-final/`.

| Check | Result | Evidence |
| --- | --- | --- |
| project_commands |5/5 passed | engine-tests.log |
| backup_restore |20/20 passed | engine-tests.log |
| worktree_on_run |1/1 passed | engine-tests.log |
| project_command_store |2/2 passed | store.log |
| proto/sync/engine check |exit0,14.93s | check.log |
| proto/sync/engine Clippy --lib |exit0,21.88s; existing warnings | clippy.log |
| focused rustfmt check |exit0 | format.log |

The28 integration cases include real disposable Git checkouts and PTYs, exact
cwd markers with shell metacharacters, failure/retry/stop, retained output across
reconnect/restart, surviving-process-group retry rejection without signaling,
setup duplication and stale-attempt fencing, both checkout-publication restart
boundaries, durable rejected/applied agent Run receipts, existing worktree reuse,
configuration CAS, task/project retirement and backup admission safeguards.
Repeat: `scripts/verify-project-commands.sh <artifact-directory>` after obtaining
an appropriate build slot. JSON artifacts include `receipts.json`,
`publication-boundaries.json` and `two-client-config-result.json`.

The actions-only stale-save baseline was reproduced before the CAS guard:
`target/qa/commands-cas-bounded-baseline/` retains invocation, log and JSON.
It proves client two silently reverted client one's Build edit and removed Dev.
The corrected readback records secondClientAccepted=false and preserves both
first-client changes. The5GiB reservation was an explicit one-test grant only;
no persistent reservation/cache settings were changed.

Earlier failed artifacts are retained under commands-initial,
commands-cas-baseline, commands-backend-50d2fad7, commands-backend-fixtures,
commands-backend-restore and commands-backend-restore-corrected. Fixture repairs
used the public flush API, correct sandbox wire spelling and the existing backup
admission contract; production backup behavior was not widened.

## Limits and remaining handoff

Supported restore was verified at the same absolute root and device using the
existing representative imported legacy-transcript fixture. Original synthetic
source data and excluded terminal output were preserved beside the recovery
root. Configuration and receipts survived; restored startup opened no terminal.
Foreign activation remains a negative case. Existing chat2 remote-frontier
reconciliation restrictions remain covered by the unchanged backup suite.

Native Settings editing, two-task navigation, output controls and Preview have
not yet been exercised. UI compilation/Clippy and the newly combined Defaults
integration checks are still pending. The isolated native identity remains:
LUNA QA Commands, `com.novnski.luna.qa.commands`, port59789, data under
`target/luna-qa-data/commands`. No Commands app has been packaged or launched yet.
