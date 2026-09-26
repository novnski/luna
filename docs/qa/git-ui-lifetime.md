# Adjacent Changes/Git lifetime matrix — before runtime edits

Runtime source: `81105db5e78297a5d9343c731de997fef3a1636b`, held at exact
`9089e97d3b42129ac078928702ff04fdf571064b` for the independent imminent25-case
PR verification/cold-preview baseline. These additions remain an unapplied
patch artifact so they cannot widen that run or mutate its source.

## Bounded scope

Use real retained Changes handlers and the unchanged generic HeldRpc helper.
Commit represents the shared stage/unstage/commit/push Git-action completion
path; no actual commit, push, filesystem restore, provider call or hosted write
is executed. Checkpoint restore exercises its independent accepted-write task.
Commit-message generation exercises a read that can fill the user's draft.

Before every transition the test observes that A accepted the exact request.
Its settlement baseline is captured before admission/transition. Accepted write
cases require request1/reply1/canceled0 and no replay. Read cancellation on a
retired context is allowed. All cases assert that B receives no write RPC.
For retired contexts, compare UI state and follow-up read counts immediately
before releasing A against those afterward. Stale completions must not issue
new reads through the current engine or for its replacement checkout.

## Independent representative cases

| Path | Engine replacement, same device/chat/cwd | Checkout replacement, same engine | Controls |
| --- | --- | --- | --- |
| Git action (Commit) | A success; A error | A success | Current-context success applies normally |
| Checkpoint restore | A success; A error | A success | Current-context success applies normally |
| Commit-message draft | A success; A error | A success | Current-context success; newer user edit remains untouched |

Thirteen cases total. This deliberately avoids duplicating each Git action enum
or every error across every transition. JSON traces retain request params,
reply/cancel classification, Git preview, staged selection, Git/restore notices,
commit-message draft, and current-engine follow-up counts.

## Source-defined failure mechanisms

- Git-action completion only checks cached git_target; same-key B replacement
  passes, and hidden checkout replacement may leave git_target at old A even
  while live git_context changed. Applying A's receipt replaces the preview,
  notices and selected paths, clears the commit draft, and starts a PR read
  through the current engine.
- Restore completion uses the same cached-target fence, displays A's recovery
  receipt/error and calls load_git through the current engine/current checkout.
- Message generation uses that cached target and an unchanged-text comparison.
  The latter correctly protects a newer user edit, but not the same retained
  draft across an engine/checkout replacement.

Do not fix these by blindly dropping shared git_task/checkpoint_task on a
retirement: accepted writes must continue once. Read busy ownership and accepted
write result ownership need explicit separation, followed by live engine and
checkout admission/completion checks. Backend journal/permission behavior stays
unchanged absent independent concrete evidence.

## Preparation status

This matrix was written before the new test source. Source-only/uncompiled;
no runtime guard or shared task-ownership change. The original22 PR cases,
3 cold-preview cases and generic helper remain byte-unchanged. Apply the patch
only after the coordinator releases the immutable25-case checkpoint, then use
an explicit granted slot for the `git_ui_lifetime_` filter. Save compiler/fixture
errors separately from behavioral assertions. No result is claimed here.

## Applying and running the prepared reproduction later

The patch adds this report, the nested test module at
`crates/ui/src/changes/git_actions/lifetime_tests.rs`, and only a cfg(test)
registration in `changes/git_actions.rs`. It does not alter runtime behavior.
It is intentionally NOT applied/registered while the exact9089e97d run is pending.

After the coordinator authorizes applying it and grants a serialized test slot:

```bash
LUNA_EPOCH_QA_ARTIFACTS="$PWD/target/qa/git-ui-lifetime-baseline" \
  CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib \
  git_ui_lifetime_ -- --test-threads=1 --nocapture
```

Use the slot's exact reservation override only when explicitly granted.
Per-case artifacts are written before assertions. The held service only returns
synthetic JSON over real memory RPC envelopes, so even representative Commit and
Restore calls never execute Git, touch a repository, or call a hosted service.
Rust formatting and patch applicability checks during preparation are not a
claim that the cases compile or that these new failures have been reproduced.

## Applied reproduction checkpoint

The coordinator approved applying the prepared patch after reviewing the exact
9089e97d run (original22 PR cases passed; cold-preview2fail/1control preserved).
The prepared patch SHA256 was verified as
`5614cda2092a06f7ef4b079b1ad1526d917a417b58fb4a7165c7bff45fbb23e6` before apply.
This checkpoint adds only the13-case test module, its cfg(test) registration and
this report. The original22 PR cases,3 cold-preview cases, generic helper and
backend journal are unchanged. Runtime remains the81105db5 implementation.
Rustfmt/diff checks pass; no Cargo/native execution or further runtime fix has
occurred. The earlier unapplied preparation artifacts and exact9089 logs/traces
remain retained for chronology and review.

## Additional ownership cases recorded before coherent runtime repair

The13-case baseline at6c486a8f reproduced nine stale completions; all eight
accepted write cases still completed once. The cold baseline independently
reproduced two retired reads keeping the loading flag. Before changing mixed
read/write task ownership, add five separate held-RPC controls without editing
any of the frozen22+3+13 cases or helper:

1. Accepted Commit + same-context Refresh: no new Git preview read and busy
   remains true until the original write completes; its result applies once.
2. Accepted Commit + Git section close/reopen: same ownership invariant.
3. Accepted Restore + same-context Refresh: same ownership invariant.
4. Accepted Restore + Git section close/reopen: same ownership invariant.
5. Accepted Commit + a newer authored commit-message draft: completion must not
   erase the newer draft. The original unchanged draft still clears normally.

Capture settlement before admission; record the while-held state before releasing
A and final state afterward. Do not assert early and abandon an accepted write.
All write requests must reply once, never cancel/replay, and no real Git/restore
or hosted operation runs. Same-context refresh may wait for the write's normal
receipt/readback; it must not cancel the write or falsely declare idle. These
new cases are source-prepared before their runtime behavior changes; no baseline
run is claimed for them during source-only work.

Planned ownership split: keep task slots cancelable/read-only, make accepted
writes retain their original engine independently, and track which current-view
operation owns busy. Context retirement clears only current-view authority;
new context reads can recover without canceling old writes. Reads/results fence
engine+selected-checkout scope and request identity. Missing-preview/error cards
must expose a usable Refresh control; a newly shown valid context can request
its current snapshot without an automatic error-retry loop.
