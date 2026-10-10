# Failure cases and E2E ownership (before backend implementation)

1. Rank keys exceed 16 bytes after repeated top/bottom insertion, rebalance changes
   existing relative order, legacy keys disappear/reorder on migration, Before/After
   ignores its neighbor, or restart changes order. Owner: `board_store_e2e` real
   RPC/SQLite/restart scenario with 1,000 insertions at each end. Existing tests
   exercise only three tickets and cannot catch the growth. No production seam.
2. Delete is missing, reuses a number, leaves a searchable/related ticket, silently
   stops active work, recreates a tombstone, drops history, or executes twice after
   retry. Owner: `board_store_e2e` plus the existing lifecycle scenario for active
   execution admission. Archive badges and delete journal must update watchers.
3. Agent image paths are rejected despite coming from its user's structured input,
   arbitrary/foreign/mentioned-only paths are accepted, stale attachment edits lose
   changes, or upload cleanup removes Board-owned bytes. Owner: `board_tools_e2e`
   through real private MCP and the existing upload/backup boundaries. No new seam.
   Launch scenario observes actual RunRequest attachment bytes in a real worktree.
4. An automation commit (agent/UI/scheduler) fails to wake the watch, subscription
   misses a commit between read and listen, or restart leaves stale sequence state.
   Owner: existing `automations_e2e` real RPC/store tests, event waits without sleeps.
5. Hidden automation lists reopen with stale rows, an old read wins over a receipt,
   or forms fail to retain saved fields. Owner: Claude's existing headless UI
   fixtures and native user flow. Screenshot evidence covers light/dark/narrow.
6. Image thumbnails/viewer use the wrong host or disappear after reopen/backup.
   Owner: native flow plus Board store backup case; same host-owned upload path
   representation as chat images. The existing attachment transfer/read and
   explicitly imported upload roots remain supported. Board itself stays local
   to the owning host (the existing PB-0 contract), not Loro replicated data.

The image E2E exposed another failed boundary before its repair: Board passed the
chosen checkout through settings resolution but discarded it while constructing
the worker's chat. A `newWorktree` start created its main worker at the original
project cwd. The image-flow scenario owns this regression too: the worker must
have a different Git checkout and receive the exact original uploaded PNG bytes.
Both fresh side and main worker creation must honor the explicit checkout; retries
reuse their deterministic checkout and chat. Main creation keeps a recoverable
checkout after failure; side creation uses the existing child-provisioning cleanup.

Deletion review found a Done ticket can have a later live user turn while still
projecting phase Complete. The completed-review E2E now verifies a new ordinary
user turn blocks deletion, and that deletion succeeds after that turn settles
without removing the chat or its later history. This case failed before the live
turn/background/queue guard and passes afterward.

Evidence lives under `target/qa/board-automation-fixes-2026-10-10/` and existing
suite artifact directories. Retain pre-fix failures and after-fix manifests/logs.
