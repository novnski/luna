# Failure cases recorded before implementation

These are E2E/integration acceptance scenarios, not unit tests added after code.
The engine scenarios exercise RPC, actual persistence and engine restart. The UI
scenarios exercise production views with real pointer/keyboard input and retain
screenshots plus machine-readable readback.

1. Main is present exactly once after every operation, save and restart. Close
   tab, close pane, close others, reopen, reset and malformed requests cannot
   remove it or turn a child into Main. Main has no close affordance.
2. Repeated Open of an already open chat focuses that exact existing tab. It
   never forks, starts a duplicate process, adds a second composer or changes
   lineage. A deliberate Move transfers the existing tab and its state.
3. Move/reorder within a group, between groups, and onto each of four split
   edges preserves every other tab once. Same-position and self-only drops do
   nothing. Drag cancellation restores the original tree and focus.
4. Nested splits have stable IDs, finite bounded ratios, valid focus, and no
   duplicate node IDs. Missing targets, reused IDs, invalid indices, excessive
   depth/size, malformed layouts and unsupported versions fail atomically.
5. Removing the last non-main tab in a group collapses that empty branch cleanly.
   Intentional empty splits have useful Open related/New child controls. The
   remaining pane expands without losing its composer or input focus.
6. Divider movement is smooth and local during the gesture; the final ratio is
   saved. Resizing the window enforces readable minima. Double-click equalizes.
   Repeated resize must not write on every pointer event or restart views.
7. Close/reopen restores the same chat, transcript, provider, options, draft,
   queued send, attachments, scroll anchor, approval and running turn. A close
   while submitting must not cancel or duplicate the send. Explicit Stop/End
   retains its existing separate semantics.
8. Related chats retains rows after open/close. Open/Open tab/Enter focuses or
   opens; dragging gives placement control. Open status is visible. Keyboard
   Reopen closed tab restores the most recent eligible item without a drag.
9. Main/Child/Side/Alongside identity stays truthful. Actual ancestry and source
   context never derive from split position. A child breadcrumb follows its real
   parent; moving it to another view does not rewrite ownership or reporting.
10. Layout and closed-tab history survive a real engine/app restart, are scoped
    to the resolved profile/main chat and never leak across accounts sharing a
    device. A stale UI callback from a replaced engine cannot write another
    profile. Switching main chats restores each independent layout.
11. Concurrent windows/clients cannot overwrite a newer layout silently. A
    revision conflict returns current state; command-ID retry does not apply
    the same close/split twice. A reused ID with different payload is rejected.
12. Persistence is atomic, bounded, under the profile directory, and obeys the
    device write-admission lifetime. Failed writes leave the prior in-memory
    and on-disk state intact. Corrupt/unknown-version files are not overwritten
    by a fallback render. Deleted chats cannot be resurrected by layout replay.
13. `+` creates one blank child with a stable idempotency key and real Main
    parent, even if another child/side pane is focused. There is no transcript
    history, synthetic user message, native session or queued turn at creation.
    First Send produces exactly one turn; closing/restarting before Send
    preserves the blank child without starting it.
14. Blank-child creation respects provider availability, real account/model
    choices, parent permission/sandbox ceilings and parent lifecycle. Side
    chats cannot become parents. Archived/deleted parents, stale scope and
    invalid settings create nothing. Retrying an accepted request returns the
    same child; conflicting payloads do not make another one.
15. Existing approved parent-agent child creation remains prompt-driven and
    starts exactly once. Manual draft creation cannot forge agent approval,
    execution authority, a worktree or outcome records.
16. Multiple visible chats route Send, Stop, approvals, queue/steering, images,
    tools, model options and drafts to their own chat IDs. Focus and move must
    not redirect any in-flight operation to the main chat or a neighboring pane.
17. Side chats retain their forked provider binding and lifecycle, including
    ended read-only history. Moving or closing a side tab never calls End.
    Reopening history does not silently start another provider process.
18. Branch and Git controls use the displayed/focused chat's actual checkout,
    with a visible title/branch context. Splits across checkouts cannot commit,
    push, run or approve against the previously focused chat by mistake.
19. Existing left-sidebar pin/order/promotion and right tools, diff, preview,
    terminal and activity workflows continue working. Layout storage does not
    reinterpret legacy sidebar placement as ancestry.
20. Light/dark themes, reduced motion, narrow windows, long titles, many tabs,
    empty/loading/error/disconnected states and keyboard navigation remain
    readable and operable. Drop previews and active focus are distinguishable
    without relying only on color.
21. Profile backup/restore carries the saved layout with its existing chat IDs,
    ratios and closed history. Layout files stay on the backup path allowlist;
    temporary files, foreign paths, symlinks and oversized records are refused.
    Restoring a layout never starts a provider or recreates a deleted chat.
22. Left/right sidebars, header and footer share one coherent frame surface in
    dark/light themes. Sidebar open/close/resize does not expose mismatched
    strips, hard seam walls, stacked right-toolbar separators, clipping or
    desynchronized animation. Every existing sidebar tool/control stays usable.
23. An intentional empty pane can be focused again after another pane, without
    choosing or moving a chat. That focus survives restart, guides a reopen
    whose original group is gone, and lets Close/New child target the correct
    group. Repeated focus is a no-op; missing panes and split-node IDs fail
    atomically. Focusing a pane never starts a provider or alters chat records.
24. A single-move drag must commit the same valid tab/Related-row destination
    and divider ratio as a longer drag. Escape before preview still cancels;
    sub-threshold travel, self-drop and divider clicks do not resize or move.
    A new press after cancellation starts an independent gesture.
25. End/archive may unmount the focused composer. The next model shortcut or
    Stop key must stay with that read-only tab and never fall back to Main,
    including when the Shell has already restored its generic fallback focus.
26. Shrinking a saved split tree with both sidebars open must not compress
    panes below their minimum size or overlap composer controls. The canvas
    can scroll, focused panes can be revealed, and gestures use their painted
    coordinates. Drops outside its visible viewport are refused. Widening
    restores the saved axes/ratios without an implicit layout write.
27. Long tab titles and footer context cannot cover Close, Split or either Git
    action. A real Review button click opens Changes bound to the focused
    chat's checkout, including an ordinary alongside chat on another checkout.
28. Opening/resizing the right sidebar or narrowing the window keeps the
    canvas's left viewport anchor; it must not hide the left panes by
    automatically revealing a focused right pane. Explicit focus/open/reopen
    can reveal its target. Manual scroll is preserved, subject to extent clamps.
29. Balance panes counts leaf panes, including empty panes, rather than tabs.
    It preserves axes, node IDs, ordering, active chats, focused pane and closed
    history, changing only split ratios in one guarded mutation. Two stacked
    panes beside one pane use 2/3 and 1/2 ratios; six mixed-axis panes have equal
    normalized areas. Existing 0.1-0.9 ratio bounds still apply to extreme
    trees. A single pane and an already balanced tree are no-ops. Replays,
    revision conflicts, restart and profile isolation retain the usual guards;
    balancing never starts a provider or changes chat identities or drafts.
