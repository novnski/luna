# Slice 3 UI: shared background work

UI owner: the established Claude conversation (`crates/ui` only). Contract:
`crates/proto/src/task_work.rs` and [slice-3.md](slice-3.md). Codex and Claude
shaped work only; OpenCode is out of scope.

## Failure scenarios (written before implementation)

1. A child's own turn ended while it (or its grandchild) still runs owned
   work, and the parent reads "Completed"; or an unread child alone is shown
   as completed.
2. A later independent message in a reused child is listed as the parent's
   owned work, offered for the parent's stop, or stopped with it.
3. A legacy child or fork with ancestry but no launch ownership offers Stop,
   or is selectable for "stop owned work".
4. A dev server (service) counts as blocking, keeps the chat busy, is
   selectable for owned-work stop, or is stopped by the current-turn Stop.
5. The current-turn Stop and the owned-work stop merge into one action, or
   the owned-work stop runs without an explicit selection and its preview.
6. A stop preview is applied after the work changed (new message, binding,
   host, deletion, terminal state, reconnect), or silently retargets.
7. A receipt's Requested reads as Stopped; Unknown is retried or resent
   automatically; Rejected/exclusions lose the host's reason.
8. A double click or reopen sends a second stop with another request id for
   the same preview, or a stop answer for an earlier connection, chat or
   preview installs into the current popover.
9. After a reconnect or stream end, the last work list still reads as live;
   historical active native work without live evidence reads as running.
10. Two panes: the chip shows in, or the popover/stop acts from, a composer
    that is not the focused chat's; a late watch frame for a previously
    focused chat replaces the current one.
11. Paging: the counts come from the loaded page instead of the snapshot's
    aggregate; a partial page is treated as everything (implicit stop-all);
    truncation is hidden; "Show more" duplicates or reorders rows.
12. More than 32 items can be selected, or selection survives a refresh for
    rows that no longer exist.
13. Long titles, details, reasons or many rows overflow the opaque popover;
    Escape or Enter in the popover reaches stop handling or the composer.
14. Native session/task ids appear in any row, detail, readback or receipt;
    `ownerTurnId`/`launchTurnId` are rendered as if they were provider ids.
15. A row's Open targets the focused chat, a same-titled chat or the wrong
    side of ownership (row chat vs lifecycle owner); Open from a pane's
    popover routes through Main or bypasses related-chat routing; a row whose
    chat is unknown/deleted offers Open or falls back to the selected chat; a
    service offers a chat instead of its project commands, or commands for
    another chat's checkout; a list read on an earlier connection navigates.

## Surface

- **Work chip** (controls row of the focused chat's own composer only): an
  icon-sized button with the aggregate blocking count; the row's budget
  reserves its width. Tooltip: "{blocking} keeping this chat busy ·
  {services} services · {unknown} unknown" (snapshot aggregates). Shown while
  the focused chat has any work or its work state is unknown.
- **Work popover** (opaque, anchored above the chip): groups **Owned by this
  chat** (selectable only when `owned && canStop`, not a service, not
  terminal), **Services** (never selectable; "Use its command controls"), and
  **Started here, owned elsewhere** (ancestry only, never selectable). Each row:
  title, state, kind, attention, host detail. Paging via "Show more" with
  totals and truncation stated. Footer: **Stop current turn** (the existing
  interrupt, only while a turn runs) and **Review stop…** (1-32 selected).
  Review shows the host preview's exact items and exclusions with reasons;
  **Stop N** sends `StopTaskWork` with a request id kept for that preview.
  The receipt lists Requested ("Stop requested; not stopped yet"), Stopped,
  Rejected (reason) and Unknown ("Not retried. Refresh to check."), with
  **Refresh** (an ordinary read). A changed/conflicting preview returns to
  selection with stale ids dropped.
- **Open** on a row: "Open chat" for a row in another known chat, "Open
  owner" for work started here but owned elsewhere, "Show commands" for a
  service (its own chat's terminal-panel project commands; if that chat is
  not the selected one, its chat opens first). The popover closes and
  dispatches `OpenWorkTarget` (task id, commands, the list's connection
  generation), which bubbles to the shell from Main's or a pane's composer;
  the shell refuses an earlier connection or an unknown chat and otherwise
  uses the related-chat routing (a workbench tab, else the chat). Rows for
  unknown/deleted chats offer no Open; native controls the host cannot reach
  stay unavailable with its reason.
- **Agents pane** ("Work · this conversation" section): the same grouped,
  read-only rows for the focused chat.
- **Child status**: "Turn completed" (from the turn outcome) instead of
  "Completed"; unread alone no longer implies completion.
- Watch: one `WatchTaskWork` for the focused chat (limit 50), fenced by task
  and engine connection; stream end or reconnect shows "Work state unknown"
  until the next frame.

## Fixture

`chat-workbench-fixture` step w20k (failure cases 48-55):

- The real host's view for the focused Main (status Live); the fixture's
  legacy children (no launch ownership) are never selectable.
- A scripted host (QA transport override; the same view, paging, preview and
  stop paths) for: an owned child and grandchild (stoppable), an owned
  workflow needing attention, a service (excluded even if the host marks it
  stoppable), a later independent child message (owned elsewhere) and a
  legacy child; aggregate blocking count 3.
- Two panes: only the focused chat's composer has the chip; Child A's
  120-row view pages 50 → 100 → 120 with aggregate counts and no duplicates.
- Explicit selection (non-stoppable rows refused), the host preview of
  exactly those rows, one request id; Requested reads "Stop requested; not
  stopped yet", Stopped reads Stopped.
- A lost answer reads unknown and is not retried; a refused stale preview
  returns to the selection without the row that ended; after the stream
  ends nothing is selectable or reviewable; Escape closes the popover without
  reaching stop handling.
- Open (failure cases 56-60): row targets (own chat, owner, service
  commands, none for an unknown chat); Open from Main's popover focuses Child
  B's tab through related routing and closes the popover; Show commands opens
  Main's terminal panel; Open from Child A's pane popover focuses Main's tab;
  a list opened before an engine reconnect is refused.

Also: the Execution card shows "Background work: …" from the same view;
legacy `ChildThread` fixture literals carry `work_origin: None`; the child
status reads "Turn completed" (child-chats-pane fixture expectation updated).
Sidebar placement, tab and sidebar badges are out of this UI slice.
