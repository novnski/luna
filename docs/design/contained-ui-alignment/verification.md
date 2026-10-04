# Verification

Source baseline: `49bd7f5f`. Evidence root:
`target/qa/contained-ui-alignment/`.

## Engine

Completed through the real RPC, managed identity, document/SQLite persistence,
provider contracts and profile restart:

- `chat_workbench`: 8 passed. The added case creates a fresh thread and an
  explicitly limited context copy from a non-Main tab, places both under the
  original Main layout, retries the exact requests and restarts the profile.
  It verifies source lineage, no history or binding for the fresh thread, no
  automatic execution, and the durable captured context of the side chat.
  `sideChatContextMode` identifies a recent-message copy before and after
  restart; the assertion failed before that metadata was published.
- `pane_chat`: 12 passed. Existing provider/account choice, fresh first-send,
  native Codex/Claude side-chat, ordinary command, draft and restart controls
  continue to pass. Native side-chat publications identify `nativeFork`.
- `task_commands::native_tip_fork_creates_independent_task_binding_and_lineage`:
  1 passed. The added publication assertion failed before the change because
  `forkSourceTaskId` was absent. It now verifies the source for native history
  forks and their descendants, source deletion, restart, unchanged binding
  identity and no additional provider fork calls.
- Engine Clippy and formatting checks passed.

The first parallel pane run timed out waiting for its Codex side-chat Stop
case (11 passed, 1 failed). That unchanged case passed in isolation, then all
12 passed in a full rerun. Both logs are retained. Restart assertions exclude
only the binding's mutable reconciliation observation and last-observed time;
provider/session IDs, generation, workspace, creation and provenance fields
remain exact comparisons.

## UI

- The existing UI library suite passed all 1,185 cases, run serially to avoid
  the known concurrent macOS font initialization failure. No unit tests were
  added.
- The appearance audit passed 810 theme/surface/contrast combinations and
  confirmed opaque tooltip and popover backgrounds. Minimum measured tooltip
  title/detail contrast was 5.99/5.19; popover detail contrast was 4.61.
- Composer, placement, related-chat and side-chat interaction fixtures passed.
  Screenshots include the opaque permission tooltip and the contained light
  chat surface.
- The expanded workbench fixture exercises creation through the actual chooser,
  tab/group keyboard focus and typing, active-tab source lineage, overlay guards,
  provider loading and removal, uncertain-create retries, source deletion,
  Settings navigation and persisted shortcut recording. Fixture input uses
  GPUI's complete keystroke/IME dispatch path. Its Mock transport adapter is
  compiled only for fixtures and converts Mock selection to the engine's legal
  inherit-parent request; production provider validation remains unchanged.
  The final run passed with 70 screenshots and 36 structured readback sections
  in `ui-attempt-6/`.
- Native QA found that macOS turns shifted brackets into brace events with no
  Shift flag. A pre-fix RPC readback confirmed that Previous tab did not move.
  Binding and conflict comparison now recognize that native representation;
  recording preserves readable Shift+[ / ] labels. The fixture dispatches
  native brace events and checks saved custom-brace migration and conflicts.

## Native acceptance

The guarded driver
`scripts/qa-contained-ui-alignment.mjs` is syntax-checked. It accepts only the
disposable `LUNA QA UI Alignment` bundle, bundle ID
`com.novnski.luna.qa.ui-alignment`, port `59807`, and its isolated data directory.

Native checks completed with the source created through the UI on Codex:

- Browsing both creation kinds and other providers, then cancelling with Escape
  or Cancel, left all chat identities, configurations, drafts, transcripts and
  provider bindings unchanged.
- Explicitly creating a New thread on Claude Code produced one fresh thread:
  no messages, initial prompt, provider binding or provider session, and the
  correct source and placement in Main's layout.
- Explicitly creating a Side chat produced one Codex native fork on the source
  provider, with its own generation-1 session and durable lineage. Its real
  reply recalled `CONTEXT_ALIGNMENT_7631` without that token being repeated in
  the side-chat prompt. Main's history remained unchanged.
- Settings opened with visible navigation while the chat sidebar was collapsed.
  Its contained surface was inspected in light and dark appearances; returning
  to chat preserved the collapsed sidebar. Native shortcut labels were inspected.
- Physical Cmd+Shift+[ and Cmd+Shift+] moved between the New thread and Side
  chat in both directions, verified by accessibility and persisted layout RPC
  readback. Trying the same chord for Save file showed the existing owner and
  left the saved keymap unchanged.
- After graceful shutdown and refresh, identities, provider bindings, drafts,
  transcripts and Main's complete stored layout matched the pre-restart
  snapshot. Startup reopened the last active conversation as its own Main;
  selecting the source reopened its saved three-tab workbench with the Side
  chat active. This check verifies stored layout recovery, not restoring the
  previous workbench host automatically.
- Final native build, UI Clippy, formatting and diff whitespace checks passed.
  The final UI library rerun again passed 1,185 cases in 8.36 seconds.

## Evidence and cleanup

The evidence root includes failure/retry logs, structured engine/native
readbacks, screenshots and archived fixture profiles. Seven fixture temporary
directories were checked for open files, archived and hashed before removal.
The exact QA bundle was quit gracefully, signature-checked and hashed, then
unregistered and removed. Its native data directory is preserved. The primary
LUNA Dev bundle and shared build caches are preserved for handoff.
The primary refresh took 3 seconds. Its exact bundle process (PID 41519), IPC
listener on port 59777 and strict deep signature verification passed. The QA
and primary bundle signing identities differ, so their signed binary digests
are recorded independently in the evidence.

Prior milestone evidence and all unrelated user data remain outside this run.
