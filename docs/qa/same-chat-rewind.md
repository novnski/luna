# Same-chat conversation rewind

Failure scenarios were recorded before implementation in the worker's external
`evidence/history-revert/failure-scenarios.md`. Validation is coordinator-owned.

Rewind removes the selected user turn and every later provider turn from the
same Codex session. Fork/edit remains a separate operation. Files are kept;
checkpoint restore is a separate previewed operation, with its own unsaved-file
and checkout token checks. Native legacy/ephemeral threads are unavailable.

## Required E2E evidence

Use an ordinary disposable Git repository, the signed single coordinator LUNA
Dev app and an explicitly authorized disposable Codex chat. Complete three
marker turns, leave an unsent draft and unrelated modified/untracked files,
then rewind before turn two. Record the task/binding/provider IDs before and
after, provider `thread/turns/list` full pages, LUNA `WatchDocMessages` reset,
`GetTaskCommandSnapshot`, receipt, draft, checkpoint list, file hashes and
screenshots of confirmation and result. IDs must remain equal; provider and
LUNA must retain exactly turn one. Continue with a fourth marker and prove
only retained history is visible to the provider. Rewind while the previous
turn's warm Codex runtime is still parked: the engine must release that idle
native writer first (no second app-server process may keep the pre-rewind
thread loaded), and the continuation must not recall the removed markers.
Restart the app and read back the same state. Rewind the first turn to prove
empty-prefix handling.

Repeat with active/queued work, a stale review, a legacy thread, a steered row,
and an external appended provider turn: expect no mutation. Inject a dropped
revert response and process interruption after staging: later rows stay
visible and sends block. `reconcileConversationRewind` reads the provider and
commits only an exact retained prefix; an unchanged original clears the
blocker without truncation, divergence remains blocked. Never replay revert.

Use the existing Changes > Checkpoints preview independently. Verify its
restore preview reports filesystem paths, prevents unsaved-file overwrite,
and creates a recovery ref, while the provider conversation is untouched.

Save JSON/RPC traces, screenshots and SHA-256 manifest outside the checkout.
Do not describe scenarios as passed until their evidence is recorded.

## Coordinator commands

After building and refreshing the signed app from the coordinator checkout:

```sh
LUNA_REWIND_CODEX_HOME=/Users/iulian/.codex \
LUNA_REWIND_MODEL=gpt-6.1-sol LUNA_REWIND_REASONING=high \
node scripts/qa-same-chat-rewind.mjs run /absolute/external/evidence/history-revert/live-run
```

Use the exact native account home configured in that app. The script explicitly
uses `codex-native`, creates its own ordinary repository/project/chat, issues
five real read-only marker turns, preserves file hashes/draft/checkpoints,
verifies native full history and same IDs, exercises duplicate delivery and
first-turn empty-prefix continuation, and writes JSON plus SHA-256 manifests.
It never builds, changes worktrees, or operates on other chats. Failed runs
retain setup IDs and RPC evidence. Leave the fixture available for UI QA.

After a coordinator-controlled graceful restart, run the read-only snapshot:

```sh
LUNA_REWIND_CODEX_HOME=/Users/iulian/.codex \
node scripts/qa-same-chat-rewind.mjs snapshot /absolute/external/evidence/history-revert/live-run
```

The snapshot compares IDs and unrelated file hashes, and retains provider/LUNA
history, draft and checkpoint evidence for review. Busy, stale, legacy, steering,
external-writer and injected uncertain/restart cases remain required scenarios;
the script's passing result does not claim these or native pixel coverage.
