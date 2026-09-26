# Project commands: failure-first acceptance

Authored before implementation. Run `scripts/verify-project-commands.sh` after
receiving the coordinator's build slot. Keep its artifact directory for review.

1. Two real disposable Git worktrees (including shell metacharacters in a path)
   run unique commands. `pwd` and marker files prove the exact selected checkout.
   No project/root/home fallback for a missing, moved, foreign or deleted task.
2. Detach subscribers, navigate to another task, reconnect and reload the engine:
   saved command config, captured command text, status and bounded terminal output
   remain available. Restore never launches commands. Terminal logs follow existing
   backup exclusions, while config and receipts use the main consistent snapshot.
3. Failure preserves exit code and output. Explicit retry creates a new attempt;
   stop cancels the owned PTY/process group. A still-live old group blocks retry.
   Invalid/empty config, duplicate action IDs, multiple setup actions and unsafe
   preview schemes fail before execution; plain task creation remains unchanged.
4. Only a fresh worktree runs the configured user-owned setup hook. Importing or
   navigating to an existing checkout does not run repository hooks. Concurrent
   setup admissions share one durable receipt. Setup success gates dependent work.
   Failure/cancellation rejects dependent work visibly; retry is deliberate.
5. A changed setup command does not rerun an existing checkout. Receipt snapshots
   include command digest and checkout generation; removing/recreating a worktree
   at the same path cannot inherit earlier success. Stale attempt completions
   cannot release a newer gate. A task moved since a run cannot stop/retry it.
6. Restart marks pending/running receipts canceled, never replays them. A PTY
   handshake prevents user code executing before its PID/attempt is durable. A
   prior surviving process group blocks retry; terminal launch/admission failures
   leave inspectable receipts. Only foreground commands are supported for stop.
7. Project deletion removes config/receipts through existing cleanup; stale or
   foreign project IDs fail closed. Writes use device admission. Backups of the
   main database include config/receipts; device ownership prevents restored
   foreign configuration becoming executable. Corrupt config is an error.
8. Native QA: editor save/edit/remove, two task navigation, run/output/stop/retry,
   preview through Luna's existing browser flow, and setup error discoverability.
   Verify isolated app bundle, PID, port 59789 and strict signature.
9. Added after source review, before any configuration-CAS implementation: two
   independent RPC clients read the same command list. Client one changes Build
   and adds Dev; stale client two changes Test from its old snapshot. The stale
   replacement must conflict, preserving both client-one changes. Capture the
   current-API baseline in `two-client-config-baseline.json` before adding the
   proposed required `expectedActions` field and comparison under the configure
   gate. No new request field is sent in the baseline reproduction.
10. Combined Projects-page composition: switching selected project or engine
    identity recreates the Commands card, so pending reads/drafts cannot cross
    project/device boundaries. Defaults controls, page route, title and scroll
    remain owned by ProjectsPage. A stale Commands save preserves its draft;
    explicit reload updates the saved list without discarding that draft.
