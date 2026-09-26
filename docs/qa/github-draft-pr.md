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
