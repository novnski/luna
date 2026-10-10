# Repository task queue acceptance

The primary owner of these contracts is `scripts/qa-pending-tasks.py`, which
drives the public CLI in real disposable Git repositories, linked worktrees and
separate processes. No Rust or GUI behavior changes in this feature.

## Failure contract, recorded before implementation

1. Two processes claim the same ticket or overlapping code areas; a worktree
   accidentally gets a separate coordination store.
2. A read of “next” is mistaken for a reservation; a paused ticket is ignored;
   unfinished work is preferred even though its dependency is incomplete.
3. A departed worker's old token can checkpoint or finish a reassigned ticket.
   An expired lease is silently reassigned while the original process runs.
4. Dependencies, cycles, missing specifications, duplicate aliases, malformed
   milestones or a stale catalog let an agent start the wrong work.
5. Progress counts prose estimates, duplicated features or deferred ideas as
   completed work. Completion accepts missing criteria or missing evidence.
6. Pause/block loses the resume instructions, partial evidence, original
   session, checkout or event history. Blocked work is automatically restarted
   while its external blocker remains unresolved.
7. A catalog update resets progress/ownership, rewrites an active contract, or
   admits an older worktree's definitions over newer ones. Concurrent intake
   loses one new task.
8. A restart or portable snapshot restores old live authority; a failed command
   partially updates the catalog/state; a corrupt snapshot is trusted.
9. A task missing from the original inventory has no intake path. Grouped
   requirements have no source mapping, or shipped Board work is reimplemented.
10. Evidence references lie about a nonexistent local file or wrong commit;
    an empty note permits handoff/completion with no useful result.

## Test authoring gate

- Observable contract: repository agents coordinate real work through the CLI,
  retain progress, resume safely and cannot duplicate claims or skip acceptance.
- Credible regressions: non-transactional selection, per-worktree state,
  unfenced writes, unconditional lease stealing, invalid graph admission and
  destructive catalog reconciliation.
- Existing coverage: no repository backlog CLI exists; Board engine suites
  cover a different runtime boundary. This single subprocess E2E owns the new
  repository queue, rather than duplicating Board tests.
- Production seams: none. An explicit state directory is also supported for
  legitimate independent local checkouts and portable recovery; the test uses
  that public option only where necessary.

## Repeatable verification

```sh
python3 scripts/qa-pending-tasks.py --output /tmp/luna-pending-tasks-e2e
python3 scripts/pending_tasks.py validate
git diff --check
```

The runner retains `result.json`, per-command JSON/readback and logs, the
disposable repository and worktree used by the run, and `sha256.json`. CI runs
the same command. A rerun requires a fresh output directory so old evidence
cannot masquerade as a current result. Inspect the result before removing only
its disposable repository/worktree; retain the logs and hashes.
