# Cold Git preview across engine replacement — failure-first follow-up

Runtime source: `81105db5e78297a5d9343c731de997fef3a1636b`.
This is distinct from the original14 stale PR-state/admission failures. The
original22 cases and generic held-RPC helper remain frozen.

## Source trace before any correction

`changes/git_actions.rs:21` starts the Git preview and owns `git_task` plus
`git_busy=true`. Its new epoch guard at43 returns on an old-engine result before
clearing busy at49. `reconcile_pr_context` retires only PR-local fields
(`changes/pull_requests.rs:125`); it does not clear/reissue the Git read.
`Changes::sync` calls that reconciler, diff watches and branch/scoped loads, but
not `load_git` (`changes.rs:2327`). Render only calls `load_git` when
`git_context` changes (`:4965`); that key omits engine generation, so a same-key
replacement does not trigger recovery.

A cold retained pane can therefore keep `git_preview=None`, `git_busy=true`
after A settles and B is attached. `render_git_card` renders loading text in this
state (`changes/git_actions.rs:670`); Refresh is inside the Some(preview) branch
at527/542. Manual close/reopen via the Git header still invokes load_git
(`changes.rs:3994`), but no observer clears/reissues the stale read automatically.

## New independent actual-Changes reproductions

Use the existing generic HeldRpc helper and a fresh actual Changes entity with
no Git preview. Hold A's GET_GIT_ACTION_PREVIEW, attach B through the real
AppState path while preserving device/chat/cwd, and settle A. Record Git busy,
preview, target/key equality, notices, and A/B request/reply/cancellation traces
before assertions. Read cancellation is permitted; stale A data is not.

1. A success after same-key B attach must not leave the cold Git loading flag
   owned by the retired request. Any populated preview must belong to B.
2. A error after same-key B attach has the same non-stall/context invariant.
3. Positive control: explicitly reissue the real load_git on B, verify B's
   preview and idle state, then settle/cancel A; A must not overwrite B.

These tests characterize the stale busy ownership without prescribing whether
the eventual recovery uses a fresh B read or an actionable idle/retry state.
A separate visible-UI acceptance must ensure a cold idle/error state has a usable
retry action; merely changing loading text would not complete native recovery.

## Adjacent source findings — separate scope, not patched here

- `git_action` (stage/unstage/commit/push) completion gates only on the stored
  Git target key (`changes/git_actions.rs:343`). A same-key B attach can admit
  A's receipt, replace B's Git preview/selected paths, write a Git notice, clear
  B's commit-message draft, and start a PR read through the current engine.
  This is an accepted Git-write UI callback finding, not proof of a replayed
  write or incorrect backend target. It needs its own failure-first lifecycle
  coverage before any correction.
- Checkpoint restore completion likewise compares only Git target at224, writes
  receipt/notice and calls load_git on the current engine at237. Treat it as a
  separate accepted filesystem/Git-write UI callback finding; no backend or
  restore behavior has been changed.
- The commit-message generator is an adjacent read/draft callback, not a Git
  write. Its key-only fence at430 plus unchanged-text check can accept an old
  engine's suggested message in a same-key replacement context.

Because `git_task` is shared with writes, the cold-read correction must not
blindly cancel that slot during engine retirement and inadvertently cancel an
accepted Git mutation. Read ownership/recovery and accepted-write result
fencing need explicit, separately validated treatment.

Source review only at this checkpoint. No new Cargo/native actions, runtime
correction or claim of an executed cold-preview reproduction.
