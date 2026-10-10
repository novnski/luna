# Five user-reported Board and automation fixes

User explicitly requested Codex backend + Claude Desktop frontend collaboration,
reading all five active tickets and the source chat before implementation.
Baseline main: `0e9416da`, clean. Source chat: “Minute-by-Minute Random Numbers”,
`d9075a5b-a18d-4e22-96dd-cffa95e70e2e`, project
`60f48b42-5ee3-4e44-aea0-cf78e7b8ad47` (this repository).

Implementation and verification are complete. See [verification.md](verification.md)
for the final scope and evidence. Claude has returned the build/test slot to Codex.

## Evidence and scope

Read the complete snapshots at
`target/qa/board-automation-fixes-2026-10-10/tickets.md` and `chat.md`.
Raw BoardList and transcript JSON are beside them. Active scope is exactly
LUN-1, LUN-2, LUN-11, LUN-12, LUN-13. LUN-3 through LUN-10 are archived disposable
examples, not implementation requests. Preserve the user's primary app data.

- LUN-1: substantially cleaner automation list/editor, readable cadence,
  shared create/edit layout, deliberate hierarchy, light/dark + narrow panes.
- LUN-2: fresh automation state after UI or agent changes, including opening
  immediately after delete; old async results cannot overwrite newer state.
- LUN-11: bounded rank keys under 1,000 top and 1,000 bottom insertions, exact
  order, Before/After/neighbour moves, legacy-key and sync compatibility.
- LUN-12: delete tickets safely through UI and agent tools, idempotent synced
  deletion with no reused numbers; live chat badges omit archived/deleted items.
- LUN-13: secure agent attachment references from user chat uploads, thumbnail
  and full-size ticket presentation, images delivered to working chats/worktrees,
  preserved through sync/backup. Attach the original screenshot to LUN-12 after
  this works (exact path is in its contextRefs and the source user message).

## Ownership and coordination

Claude owns `crates/ui/**` and `frontend-status.md` here. Codex owns everything
else, including backend contracts, root tests/scripts/docs, all Cargo commands,
app slots, native QA and Git. You are not alone: preserve the other owner's edits.
No subagents. Use High effort. No edits while a Cargo/test run reads the checkout;
Codex explicitly owns and announces freeze/build windows. Claude does not build,
launch/quit apps, commit/push, or mutate primary Board tickets.

Use `.codex/skills/test-audit/SKILL.md` before tests; failures first and strongest
E2E boundary, no new test-only production seams. Retain before/after screenshots
and machine-readable readbacks. Follow the existing LUNA design tokens/fonts,
controls and accessibility patterns. The better-design skill should improve the
existing native system, not replace its typography or global theme.

## Frontend direction

Person/action: Iulian quickly reads schedules and adjusts an automation without
scanning repeated technical paragraphs. Feel: calm, compact, clear native LUNA.
Domain: schedule, cadence, destination, next run, status, model, delivery history.
Color world: current theme's graphite/cream, muted text, accent, success and danger;
use semantic tokens and respect the selected accent. Signature: a concise human
summary of when/where the automation runs, with status and actions immediately
visible. Avoid equal-weight stacks of boxed sections, duplicated destination/access
text, and oversized form rows. Group Essentials, Schedule, Destination/Model and
secondary Notifications/Lateness controls; choose sensible progressive disclosure
without hiding review errors or saved values. Keep full-page and right-pane usable.

## Shared interfaces

Canonical additions (Codex owns their implementation):

- `WatchAutomations` takes `{}` and streams `AutomationWatchFrame { seq: u64 }`.
  Initial frame always arrives. Every committed automation definition/run change
  bumps seq and invalidates the whole local-host snapshot; coalescing is allowed.
  Seq is monotonic within an engine generation; reconnect resets the generation.
  Subscribe once per engine and refresh visible lists and cards on frames. Keep
  reveal revalidation and revision floors as defense against in-flight replies.
- `ChatTicketBadge.archived: bool` (serde default). Archived related tickets must
  disappear from source-chat banners/counts. A working chat may retain its own
  archived badge. Deleted tickets are absent from all badge responses.
- `BoardDelete` takes `BoardDeleteRequest { request_id, work_item_id,
  expected_revision: u64 }`; returns the existing `BoardMutationReceipt` with
  operation `Delete` and `item.deleted == true`. `WorkItem.deleted` is default false.
  A permanent tombstone retains key/history/receipts and is excluded from reads,
  lists/search/counts/badges. No restore and no key reuse. Reject Ready, active or
  uncertain worker/review states; user must stop and wait first. UI must confirm:
  delete the ticket, preserve chats/files/Git/external issues. Active links are
  retired, dependency edges removed; old launches/reviews/events retained.
  Delete emits one journal event, including to AllProjects watchers. Agent tool
  `board_control` action `delete` uses the same revision and safety rules.
- Ticket images retain existing `WorkItem.attachments: Vec<String>` of host-owned
  upload paths. No replacement attachment model needed by UI. Add
  `BoardItemChanges.attachments: Option<Vec<String>>` for UI edit round-trip;
  attachments are a revision-tracked brief field. Render with existing chat image
  thumbnail/full-size mechanisms and host routing. Agent `board_create.attachments`
  and `board_update.changes.attachments` accept upload paths, but engine authorizes
  only structured attachments from the bound chat's persisted commands (or bound
  authoring session), never paths merely mentioned in message text. Same project
  and owning host restrictions still apply. Working launch already carries images.
- Rank keys become fixed-width, at most 16 characters, board-only. On first rank
  mutation of legacy data, normalize the entire column in exact existing order;
  rebalance transactionally when a neighbor gap is exhausted. Reorders preserve
  content revisions and never schedule execution. No global sidebar-rank changes.
