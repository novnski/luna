# GitHub draft PR increment: failure-first acceptance

Authored before implementation. No live GitHub writes or pushes are authorized.

The integration binary exercises the production service, real Git subprocesses,
a disposable checkout, and an executable fake gh. The native acceptance uses the
same fixture responses in LUNA QA Pull Requests (59790). Capture form, linked
PR details/checks, failure and retry states. Confirm title/body/base/head preview,
no write before explicit Create draft PR, preserved input on failure, navigation
isolation, and existing Luna theme spacing/color/type/borders.

Failure cases:
- No gh or authentication: actionable error, zero writes; retry read succeeds.
- No upstream or unpublished head: push branch first, zero writes.
- Divergent fetch/push URL, upstream branch/remote mismatch: refuse, zero writes.
- Remote/head/upstream changes after preview, edited head mismatch, invalid base,
  or same base/head: refuse, zero writes.
- Existing PR: exact host/repo/base/head only; return existing without a write.
  Another fork/repo/base/head must never be returned as this attempt's PR.
- Success: explicit draft POST, exact repo/head/base/title and multiline body;
  read back exact PR details and check statuses. No push, merge or review API.
- Provider accepts then loses response: reconcile exact PR; one write total.
- Provider fails without trustworthy outcome: durable pending state; repeat and
  reconstructed service can only reconcile, never send a second write.
- Cancellation while POST is pending: persisted pending record survives restart.
- Read timeout/output cap and malformed response: visible failure, no invented
  no-PR/success result; read-only retry remains possible.
- Concurrent submit: serialize through existing checkout lane and durable
  exclusive attempt creation across checkouts; no duplicate writes.
- Backup/restore and retired profile must preserve attempt safety.

Repeatable command (only after granted build slot):
`scripts/verify-github-pr.sh`. Artifacts remain under target/qa/github-pr.*.

Scope: same selected GitHub repository only. Fork-to-upstream creation, implicit
push, hosted reviews/merge, inbox and non-GitHub forges are excluded. Local diff
comments retain their existing local behavior.

## Design/source references

Reference checkouts were read without moving HEAD: T3 HEAD
`e5a46d6c5d00b89afba5274a94d42428c8d79763`, fetched `origin/main`
`2a9832b8019039f812706639c11ccc24ce4c4e05`; BB HEAD
`9c8112e546f042b923f1a0c080f6b427c2fa60f6`, fetched `origin/main`
`9c9bae7f36a237c7e1b96de3d4c2186d13967686`.

T3 `apps/server/src/git/GitManager.ts` at the fetched revision connects create_pr
to its stacked Git action phases (around 2641) and includes automatic push.
This increment deliberately requires the user to publish using Luna's existing
Git actions first. BB `plugins/github/app.tsx:1296` supplies a compact checks
section. Luna's existing Git card tokens/buttons/ComposerInput and linked badge
are reused; review comments retain local prompt handoff behavior.

The CLI fields were checked against a harmless actual read of `cli/cli` PR14519:
headRepository has name; headRepositoryOwner has login; check rollup contains
CheckRun status/conclusion/name/detailsUrl. The local Luna repository returned
no PRs. No GitHub writes were used for verification.

API references: [create pull request](https://docs.github.com/en/rest/pulls/pulls#create-a-pull-request),
[gh pr view fields](https://cli.github.com/manual/gh_pr_view).

Native fixture preparation: `node scripts/prepare-github-pr-qa.mjs` creates only
`target/qa/github-pr-native/checkout` and writes a launch environment JSON. After
building/refreshing the assigned unique app, run the script with
`--restart-and-seed`. It gracefully quits the verified QA PID, proves that
process exited and port59790 is free, then uses `open -n --env` to start a fresh
QA process with the fixture environment. It verifies LocalOwnerRootClaim and
the exact bundle before adding one synthetic project/task through port59790. The fixture gh requires its
fixture directory and never accesses the network. Change the `mode` file to
`auth`, `invalid`, `oversized`, `diverged`, `reject`, `lost`, or `success` for
repeatable UI checks. Preserve QA data while the coordinator reviews the app.

Additional acceptance authored before journal lifecycle change: a confirmed
closed or merged PR permits a new explicit reviewed operation UUID, repeated
old operation IDs return their prior PR (including terminal state), concurrent
rollover writes once, and restart/backup retains all prior attempt evidence.
An unconfirmed attempt remains reconciliation-only even with a new form UUID.
Lock sidecars are coordination-only, auto-unlocked by the OS and excluded from
backup/restore evidence.

Journal version2 keeps at most256 operation records per normalized target. It
never evicts identities to admit another write; reaching the cap gives an
explicit error. Each operation stores SHA256 of normalized create content,
not plaintext title/body. Reusing the same UUID with changed content is rejected.
A true retry returns its confirmed PR even after closure. Advisory lock files
remain on disk and are never unlinked while live; the OS releases their locks.
They are omitted from archives and restored-state fingerprints.

## Verification checkpoint — 26 September 2026

Base merged: `dbb38ee14d6913bc6e79f6de2bd33be398b3d839` (merge `b9c8273d`).
No primary checkout edits, hosted GitHub writes or pushes. Shared settings fixture
repair was cherry-picked as `6023afe3` (same change as `65c3800d`).

Evidence directory: `target/qa/github-pr-initial/`.

- `before-fix.log`: authentic advanced-branch regression; compilation passed
  in52.39s, test failed at the old-UUID replay (`Rejected` vs `Existing`).
- `after-fix.log`: advanced-branch replay passed; a separate restore-fixture
  identity mismatch failed. A different device legitimately needs a fresh
  checkout preview; the fixture now asserts that rejection first, then retries
  with refreshed checkout identity and the original operation UUID.
- `final-integration.log`, `result.json`, `calls.jsonl`: production service,
  real disposable Git repo and fake-gh subprocess acceptance.26 named cases
  cover create/readback/checks, target guards, auth/CLI failures, bounded output,
  cancellation, independent lane OS-lock contention, uncertain outcomes,
  restart/restore, changed UUID payload, terminal reuse and advanced-SHA replay.
- `backup.log`: journal archive/stage byte preservation and absence of lock
  sidecars passed1/1. `ui-check.log`: UI+engine compilation passed43.62s.
- `clippy.log`: engine/UI library Clippy exit0, existing repository warnings;
  a new collapsible-if style warning was fixed and rerun in
  `engine-clippy-final.log`. `format.log`: workspace formatting check exit0.

The ordering correction retains checkout/repository/branch/upstream and payload
identity checks for replay. Known operation IDs cannot reach POST. New UUIDs
still require the reviewed Git token, live published SHA and final target check.

Native app and screenshot QA remain a separate coordinator-gated step. This
checkpoint does not claim the UI has been visually verified or an app refreshed.

### Native environment preflight failure cases (before driver change)

The later native run must fail closed before any PR-create action if the IPC
listener is not the exact QA executable, bundle ID/port/data root differs,
LocalOwnerRootClaim or device-id readback differs, the QA process refuses graceful
quit, the old PID survives, or the new process cannot prove fixture-gh usage.
The driver must not send a PR-create RPC. Seed mutations occur only after owner
claim verification. A harmless fresh preview must append exact-repository/branch
fake-gh calls without increasing fixture POST count; save that proof with PID,
root claim, timestamp and operation ID. AppKit quit targets a verified QA PID
and executable, never an app name/main process. An existing process cannot be
reused for injecting the fixture environment. Main IPC PID must remain unchanged.


Native launch sequence (only after the coordinator grants the native slot):

```bash
export CARGO_INCREMENTAL=0
export LUNA_DEV_APP_NAME='LUNA QA Pull Requests'
export LUNA_DEV_BUNDLE_ID='com.novnski.luna.qa.pullrequests'
export LUNA_DEV_IPC_PORT=59790
export LUNA_DEV_DATA_DIR="$PWD/target/luna-qa-data/pullrequests"
scripts/run-macos-dev.sh --refresh
node scripts/prepare-github-pr-qa.mjs --restart-and-seed
```

Before any UI Create action, inspect
`target/qa/github-pr-native/native-preflight.json`: `passed` must be true for the
current exact QA PID, root claim and bundle. It records fresh read-only
fake-gh calls for `repos/acme/luna` and `branches/feature%2Fpr`, the preview's
operation ID, and unchanged fixture POST count/main PID. `--seed` can recheck
and reseed an already verified fixture process but cannot inject its environment;
it fails without fresh fake-gh proof. A refusal to quit never triggers a force
quit. No PR-create RPC is present in this driver.

This preflight driver change was syntax-checked with `node --check` and reviewed
against the live Rust root-claim algorithm. No native launch/build was performed
while preparing it; actual runtime readback remains gated.
