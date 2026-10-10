# Frontend status (Claude, `crates/ui/**`)

Baseline `0e9416da`. Claude edits only `crates/ui/**` and this file; no Cargo,
app slots, commits or primary-data writes. Codex owns builds/QA/Git.

## Edit window

- State: **FINAL — verified, IDLE (related tickets incl. focused side
  chats).** No Claude writes; Codex owns final native readback,
  `verification.md` and commit/push. Side-chat proof
  (`target/qa/related-tickets-sidebar-2026-10-10/`): `side-before/
  manifest.json` 31 checks with the one expected failure
  (`focused-side-chat-tickets`); `side-final/manifest.json` 31/31 PASS;
  `related-chats-final` PASS; luna-ui `--tests` check PASS (20.10 s), build
  52.33 s, Clippy PASS; `f12b-side-tickets.png` inspected by Codex (side-
  owned ticket, contextual heading, chat library preserved). No remaining
  side-chat limitation: a focused side chat's related tickets are listed
  (and counted) in the right sidebar under its own heading, and the main
  chat's return when its tab is focused.
- Freeze rule: when Codex announces a build, Claude stops editing and sets
  this line to FROZEN until Codex reopens it.

## Related tickets → right sidebar (correction after `dfba2f00`)

User: five large "Related ticket" banners above the source conversation are
pointless; related tickets belong in the right sidebar. Evidence root:
`target/qa/related-tickets-sidebar-2026-10-10/`. Backend contract unchanged
(`BoardChatBadges.related`, `archived`, deleted omitted; watch refresh).

Where: the existing right-pane **Related chats** surface
(`shell/related_chats.rs`), which already follows the selected chat
(`active_chat`). A compact **Tickets** section leads its list: one dense row
per active related ticket (glyph · `KEY` · title · column/phase), deduplicated
(engine related ∪ this session's Created receipts, minus the chat's own
working/reviewer ticket, minus archived/deleted), no prose or per-row
buttons; the row itself opens the exact ticket (`chat_open_ticket`). The
picker row "Related chats" counts tickets too. The pane never opens or takes
focus when tickets arrive.

Main conversation strip (`chat_badges/render.rs`): keeps only real identity
(working `Ticket KEY` + status control, `Reviewer · KEY`); no related rows,
no "N more", no Created/Converted/Hand-off receipts (their transient
sidebar notice stays). Receipt data stays as the source of immediate rows
and its reconciliation is unchanged.

Files (6): `chat_badges/render.rs`, `chat_badges.rs` (merged ticket list
helper), `chat_badges/dialogs.rs` (stop pushing receipts that have no
remaining surface), `related_chats.rs` (section + empty state + probes via
the existing `board_probe_child`/`board_mark`), `shell.rs` (picker count),
`board-chat-ui-fixture.rs` (E2E owner). No new QA-only seams: probe labels
use the existing board probe; the fixture opens the pane through the
existing `fixture_open_surface("related")`.

Probe contract the fixture asserts (written first):
- row bounds `related-ticket:{chat}:{workItemId}`; section bounds
  `related-tickets:{chat}`; list viewport `related-chats-viewport`;
- mark `strip:{chat}` never contains "Related ticket", "more related" or
  "Created"; no `receipt-open:`/`receipt-goto:` bounds in any frame.

Label (user follow-up): the picker row and the pane's tab/title read
**Related** with the description "Chats and tickets" (internal
`RightSurface::RelatedChats`, settings and readback keys unchanged). No
fixture asserts the old label text (only comments mention it).

Pre-fix run (Codex): `scripts/cargo.sh build --locked -p luna-ui --features
appshots-fixture --example board-chat-ui-fixture`, then
`target/debug/examples/board-chat-ui-fixture
target/qa/related-tickets-sidebar-2026-10-10/ui-before`. Expected on
`dfba2f00`: `no-related-tickets-in-conversation` fails (Created receipt +
related rows in `strip:SOURCE`), `related-ticket-in-right-sidebar` and
`related-row-opens-ticket` fail (no `related-ticket:*` bounds),
`conversation-unobstructed`, `all-related-tickets-reachable`,
`related-follows-selection`, `lun12-*-sidebar` fail; `create-ticket-from-chat`,
`create-keeps-the-pane-and-focus` and the earlier stages pass. Missing rows
are recorded, not fatal, so one run reports them all.

Implementation (uncompiled):
- `chat_badges/render.rs`: the conversation strip renders only the chat's
  own identity row (working `Ticket KEY` + status control, or `Reviewer ·
  KEY`); `render_board_receipt`, `render_board_related`, `receipt_row` and
  `RECEIPTS_SHOWN` are gone.
- `chat_badges.rs`: session receipts (`ChatReceipt`, `ReceiptKind`,
  `receipts`, stamps, `read_seq`, `push/dismiss_receipt`) removed — they had
  no surface left. A created ticket is inserted into `related` at once (as
  before); the dirty-read discard keeps an in-flight older snapshot from
  dropping it; Archived/Deleted events and the `deleted` set still remove
  rows live; snapshot install still filters archived/deleted.
- `chat_badges/dialogs.rs`: Created/Converted/Hand-off no longer push
  receipts (their transient sidebar notices stay); `DialogOutcome::HandedOff`
  drops its unused title; a create answered after a delete is not listed.
- `chat_badges/fixture.rs`: readback drops `receipts` (existing QA readback).
- `related_chats.rs`: `render_related_tickets` leads the list: header
  "Tickets (N)" (sidebar section header) and one 30 pt row per ticket
  (ticket glyph · KEY · title · column or phase), the row a button opening
  `chat_open_ticket`, accessible "Open ticket KEY: title. Column · Phase",
  and a hover `ControlTooltip` ("KEY · full title", detail "Column ·
  Phase") for titles truncated in a narrow pane.
  The empty state shows only with no chats and no tickets. The list sits in
  a non-scrolling frame probed as `related-chats-viewport`; rows/section
  use `board_probe_child`/`board_mark` (existing probe). Title "Related ·
  {chat}".
- `shell.rs`: tab title "Related"; picker row "Related" + "Chats and
  tickets", count = open related chats + related tickets, accessible label
  names both; right-pane + menu row "Related".
- Fixture: `strip_tickets` now reads only `board.related` (receipts are
  gone).

After-run repair (fixture only): `lun12-archive/delete-leaves-sidebar`
failed on stale probe bounds, not the product — in the failing frame the
archived `wi-f57…` row kept bounds at y 122, the slot `wi-98d…` had moved
into, while the section was 156 pt (header + 4 rows) and
`f13-related-archived.png` shows 4 rows. `wait_for` polls without clearing
the probe. New `wait_fresh` clears the probe and draws before every poll;
f13's painted-row waits (all rows, back to SOURCE, archive drop, unarchive
restore, delete drop) use it. Assertions unchanged.

Focused side chat tickets (required follow-up; tests first):
- Plan: the Tickets section and the picker's ticket count resolve their
  owner from the focused conversation (`workbench_execution_focus`, else
  the selected main chat). The Related list's chats keep MAIN's library
  (unchanged). When the owner isn't the list's main chat the section reads
  "Tickets · {side chat title} (N)" and marks
  `related-tickets-context:{owner}`. Row accessible title uses
  `single_line`. Probe labels key rows by the owner:
  `related-ticket:{owner}:{id}`, section `related-tickets:{owner}`.
- Fixture f12b (failure case 21), after f12's Go to chat leaves the side
  tab focused in MAIN's workbench: Create ticket from the side tab's menu
  (`menu:tab:create-ticket:{side}`, real dialog + engine); open Related;
  `focused-side-chat-tickets`: the side chat's rows (= its related data),
  mark `Tickets N`, a context mark naming it, no MAIN rows, nothing in the
  conversation. Focus MAIN's tab: `focused-main-chat-tickets`: no side
  rows, MAIN's rows equal MAIN's related data, no side context mark.
- Expected before-fail on the current tree: `focused-side-chat-tickets`
  fails (the section follows MAIN, no context mark); everything else
  passes.
- Before-fail (Codex): `side-before/manifest.json` 31 checks, only
  `focused-side-chat-tickets` failed (side ticket in data, no rows/count/
  context painted).
- Implementation (uncompiled): `related_chats.rs` adds
  `related_tickets_owner()` = `workbench_execution_focus()` else the
  selected main chat; `render_related_tickets(owner, parent)` lists the
  owner's tickets, keys row/section ids and probe labels by the owner, and
  when owner ≠ list main names it: header "Tickets · {chat} (N)", group
  label "Related tickets of {chat}, N", mark
  `related-tickets-context:{owner}`. Row accessible title now
  `single_line`. `shell.rs` picker ticket count uses the same owner. The
  Related chat library (parent, sections, side-chat button) is unchanged.

Failure cases (board-chat-ui-fixture f11/f13, before implementation):
14. A related ticket or creation receipt paints in/above the main
    conversation (strip marks or `receipt-*` bounds).
15. Creating a ticket opens the right pane or moves focus.
16. In the right sidebar's Related chats, a related ticket is missing, listed
    twice, or paints outside the pane (left of the conversation's composer).
17. With 5+ related tickets any is hidden behind "N more" or unreachable by
    scrolling the list.
18. Selecting another chat keeps SOURCE's tickets; returning doesn't restore.
19. A row doesn't open that exact ticket on the Board.
20. Archive/delete doesn't remove the row (and unarchive doesn't restore it)
    without reopening.

## Plan

1. LUN-1 (frontend only, starting now): redesign the list row and the
   create/edit form in `automations.rs` on the existing settings kit
   (`section_card`, `card_row`, `meta_line`, badges, `button`), same tokens
   and fonts. Shared layout for create and edit; works in the page and the
   narrow right pane, light and dark.
2. LUN-2 (UI part now, live part after the watch contract): revalidate on
   reveal, revision floor for receipts vs. reads, then subscribe to the
   backend watch.
3. LUN-12 banner: filter archived related tickets and count once the badge
   carries the flag; refresh badges on watch resets. Delete UI waits for
   the canonical delete types.
4. LUN-13: ticket detail thumbnails + full-size viewer reusing chat
   attachment rendering (`attachments.rs`, `image_viewer.rs`) once the
   canonical attachment reference type lands.
5. Coverage: extend `crates/ui/examples/chat-workbench-fixture.rs` w20m/w20n
   (real engine, real RPC) with failing-first readbacks and screenshots;
   no new production seams beyond the existing `fixture_*` QA surface.

## Findings

### LUN-1 (current UI)
- Each row stacks 7–9 equal-weight 12 pt lines: status, cadence + zone,
  destination, execution, notifications, "Runs on … · access up to … ·
  revision N", source chat, PR watch. Revision and access ceiling are
  implementation detail in the primary scan path.
- Actions: History, Edit, Pause/Resume, Delete as four equal Secondary/Ghost
  buttons; status is only a small pill.
- Editor: one flat card with ~12 equal rows (name+prompt, project, fresh
  switch, chat, a destination sentence that repeats the rows above, model,
  repeat chips, schedule fields, notifications, archive, lateness, review).
  No grouping; lateness is a full-width row with a 55-character label.
- "Every 1 minutes" comes from the **engine's** `automation::describe_trigger`
  (`crates/engine/src/automation.rs:236`), used in agent-tool receipts
  (`rpc/automations.rs:202`). The UI's own `describe_trigger` already says
  "Every minute". → Codex: humanize the engine copy (1 → "Every minute",
  60 → "Every hour", multiples of 60 → hours, 1440 → "Every day").

### LUN-2 (stale automation list) — root cause
- The Automations **page** panel (`Shell::automations_settings`) is created
  once and never dropped; leaving and reopening the page reuses it.
  `set_scope` is a no-op for the same scope, so the reopened page paints its
  cached `first` rows. The only re-read is the 5 s poll, which runs only
  while the panel rendered in the last 6 s. An agent delete while the page
  was hidden therefore stays visible for up to ~5 s after reopening. Exactly
  the reported "couple of seconds".
- After a UI Pause/Resume/Save/Delete, the receipt's record updates cards
  and the pinned row but not the listed row; the row keeps the old state
  until the follow-up read lands (a visible flip-back window).
- `AutomationCards` (inline chat cards) re-read only every 30 s and accept
  any answer without a revision check, so an older read can overwrite a
  newer receipt.
- UI fix (no backend needed): revalidate on reveal (hide cached rows until
  the fresh first page lands), patch rows from receipts immediately, and keep
  a per-id revision floor so no read or receipt with a lower revision
  replaces a newer one (rows and cards).

### LUN-12 banner — root cause
- `board_chat_badges` (`crates/engine/src/board/chat_identity.rs:531`) never
  checks `item.archived`, and `ChatTicketBadge` has no archived flag, so the
  UI cannot filter. The UI groups whatever arrives (`chat_badges.rs:336`).
- Refresh path: badges re-read on `BoardModelEvent::Events` from the
  AllProjects `BoardWatch`. A frame with `reset` but no events does not
  re-read badges (UI fix, mine).

## Contract requests to Codex (minimal)

1. **Automation watch** (LUN-2 live updates): a subscription, e.g.
   `WatchAutomations` (same shape as `WatchQueue`), that emits a frame after
   every committed definition change from any actor (UI RPC, agent
   `automation_update`/MCP, scheduler completing a one-shot or advancing
   `next_due_ms`): `{ "seq": u64, "automationIds": [String] }`, `seq`
   monotonic per host, one initial frame on subscribe. Emit after commit, so
   a read issued on receipt sees the change. The UI then invalidates and
   re-reads with a newer `load_seq` (older answers are already dropped), so
   pages need no seq of their own. Reconnect is already handled by
   `engine_generation`.
2. **Badge archived flag** (LUN-12): `ChatTicketBadge.archived: bool`
   (`#[serde(default)]`), or exclude archived items from `related`. Prefer
   the flag: the UI hides archived related tickets from the banner and count,
   keeps a working chat's own badge (marked archived), and can offer "Show
   archived". Deleted items must be absent from `BoardChatBadges` entirely.
3. **Board journal** (LUN-12): archive, unarchive and delete each produce a
   journal event in AllProjects `BoardWatch` frames (delete included even
   though the item is gone), so badges refresh live.
4. Waiting for: canonical ticket delete request/receipt/error types and
   rules; canonical ticket attachment reference type (LUN-13). Not invented
   here.

## Checkpoint 1 (uncompiled; rustfmt + `git diff --check` clean)

Files: `crates/ui/src/automations.rs`, `crates/ui/src/shell.rs` (one field,
its init, one call in `Shell::render`, three mount sites),
`crates/ui/src/shell/automations_pane.rs`.

- LUN-2 UI: `AutomationsPanel::reveal` + `Shell::automations_outlet` /
  `automations_frame_begin`: a panel mounted again after a frame without it
  hides cached rows (`revalidating`, skeleton) until its fresh first page
  lands. `apply_receipt` patches/removes the row at once; `receipts` +
  `newest()` keep a per-id revision floor so an older read never restores a
  previous state. `AutomationCards` reject older revisions too. Readback adds
  `revalidating`; `rows` is now what the list shows.
- LUN-1 row: tile + name + lifecycle; summary line in primary tone
  (`Every minute · In this chat · Next 14:03 · in 2 min`); quiet meta line
  (model · notifications · archiving); actions Pause/Resume, Edit, History,
  Delete as a trash icon (confirm unchanged). Host/access/revision/time
  zone/lateness/source move to a label–value Details block shown with
  History. Interval wording: hours/days for multiples of 60/1440.
- Removed `status_detail` (only the old row used it).

## Checkpoint 2 (uncompiled; rustfmt + `git diff --check` clean)

Contract use: `methods::WATCH_AUTOMATIONS` + `luna_proto::AutomationWatchFrame`,
`ChatTicketBadge.archived`, `WorkItem.deleted`, `WorkItemEventKind::Deleted`,
`BoardDeleteRequest` / `methods::BOARD_DELETE` → `BoardMutationReceipt`,
`WorkItem.attachments` (unchanged model). Every UI `BoardItemChanges`
literal already ends in `..Default::default()`; no UI literal of
`WorkItem`/`BoardFieldRevisions` exists; no exhaustive UI match on
`BoardAction`/`BoardOperation`.

Files:
- `crates/ui/src/automations.rs`
  - LUN-1 editor: one layout for create/edit. Groups: name+message;
    Schedule (kind chips, fields, live summary "Every minute ·
    Europe/Bucharest" or the first unreadable field); Where it runs
    (destination sentence once, Project select, New/Existing chat segmented,
    Chat select); Model (pickers + note); More options (collapsed summary
    naming notifications/lateness/archiving; opens itself on a lateness
    refusal); review footer on a recessed row; next runs as one line.
    `editor_schedule()` extracted from `build_definition()` (shared parsing;
    timezone/trigger errors now precede the lateness error).
  - LUN-2: `AutomationWatch` global (one `WatchAutomations` stream per
    engine generation, retries on transport loss, idle on older engines).
    Panels re-read on every frame (later pages dropped so a deleted row
    there can't linger); cards invalidate and re-read. Readback adds
    `watchSeq`.
- `crates/ui/src/shell/board/chat_badges.rs`: related tickets with
  `archived` are filtered from the strip and count; an `Archived`/`Deleted`
  journal event drops that ticket's session receipts and related rows at
  once (the second leak: receipts kept archived tickets in the banner).
- `chat_badges/render.rs`: an archived working badge says "· Archived".
  `chat_badges/dialogs.rs`: `archived: false` literals. `palette.rs`:
  "Deleted" activity label. `chat_badges/fixture.rs`: badge `archived`.
- `crates/ui/src/shell/board/model.rs`: a reset frame emits
  `Events(vec![])` so badges re-read; a deleted receipt removes the card.
- `crates/ui/src/shell/board/api.rs`: `delete(BoardDeleteRequest)`.
- `crates/ui/src/shell/board/detail.rs`:
  - Delete: row under Archive; offered only with `BoardAction::Delete`;
    Ready/running shows why it can't. In-place confirm (Dialog role,
    Escape cancels) states permanence, key never reused, and that chats,
    files, Git branches and GitHub issues are kept. Success closes the
    drawer and drops the card; a `Deleted` event for the open ticket closes
    it too. Readback: `confirmDelete`.
  - LUN-13: brief images as 112×80 thumbnails (shared chat attachment cache
    and loader keyed by `item.owner_host_id`, relayed when remote), opened
    in the chat lightbox. Readback: `images[{path,state}]`, `imagePreview`.
    No attachment add/remove UI yet (create form already uploads).

## Verification commands for Codex (build/test owner)

```
scripts/cargo.sh check --locked -p luna-ui --tests
scripts/cargo.sh check --locked -p luna-ui --features appshots-fixture --examples
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture --example chat-workbench-fixture
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture --example board-chat-ui-fixture
target/debug/examples/board-chat-ui-fixture <ABS_EVIDENCE_DIR>
```
Run `chat-workbench-fixture` the way CI/QA already does (its header documents
the output dir). New stages and artifacts:

- `chat-workbench-fixture` w20m, after case 72 (paging), before
  `fixture_leave_settings`:
  - LUN-2 hidden path: leave the page, delete `w20m-bulk-*` row #1 through
    `PreviewAutomation`/`MutateAutomation` on the reader (host path agents
    use), reopen; every painted frame must exclude it →
    `w20m-lun2-reveal.json` (per-frame `revalidating/loading/rows/
    deletedShown/watchSeq`), `w20m-lun2-reopened.png`. Fails on baseline
    (cached rows painted until the 5 s poll).
  - LUN-2 visible path: delete row #2 while the page is shown; must vanish
    in < 4 s (watch, not poll) → progress key `lun2StaleAutomations`.
  - LUN-1 captures: `w20m-lun1-list-light.png`, `w20m-lun1-list-dark.png`;
    editor captures stay `w20m-automation-editor-pane.png` (narrow pane) and
    `w20n-automation-editor.png` (page).
- `board-chat-ui-fixture` f13 (new failure case 13), at the end: two more
  tickets via SOURCE's "Create ticket"; `BoardArchive` one → strip and
  count drop it live (`lun12-archive-leaves-strip`); unarchive → back
  (`lun12-unarchive-returns`); `BoardDelete` another → gone and receipt
  `item.deleted` (`lun12-delete-leaves-strip`). Captures
  `f13-related-three/archived/deleted.png`, readbacks `rb-13-*`. Fails on
  baseline (archived related and receipts stay).

Not yet covered by a UI fixture (next window): detail-drawer Delete confirm
click path and the brief thumbnail/lightbox (needs a seeded upload on the
owning host; `board-detail-fixture` is the owner).

## Checkpoint 3 (uncompiled; rustfmt + `git diff --check` clean)

Review findings (all three fixed):
- **Cards in-flight race** (`automations.rs`): `AutomationCards` has a
  `read_generation` bumped by every watch frame; `invalidate()` also drops
  the in-flight read tasks (cancelling them), and an answer from an older
  generation is discarded before it can mark a card fresh. Card answers and
  `observe_records` compare `freshness()` = `(revision, updated_at_ms)`.
- **Panel floor** (`automations.rs`): `latest` is a monotonic per-id floor
  from reads *and* receipts (never removed; cleared only on reset/scope
  change). A read applies at `>=` freshness and raises the floor; a receipt
  applies only at strictly greater freshness, so a delayed receipt can't
  roll back a newer read or a scheduler-advanced `next_due_ms` at the same
  revision.
- **Badge snapshot races** (`chat_badges.rs`, `dialogs.rs`):
  - an in-flight badge read that a board event dirtied is discarded and
    re-read before installation (no momentary restore);
  - each read gets a number; receipts are stamped with the latest read
    number when pushed; a full snapshot from read N retires Created /
    Converted ticket receipts stamped before N whose ticket no chat shows
    (deleted or archived-related, including after a missed journal gap /
    reset). Hand-off and ticketless receipts stay;
  - tickets seen `Deleted` are remembered for the session: a late create
    receipt or related row for them is never shown; a Created receipt
    triggers its own badge read so an archived-meanwhile ticket retires.

LUN-13 brief image editing (`detail.rs`): the brief editor lists its images
(saved thumbnails from the shared cache, staged ones from memory), each
with a Remove (×) control, plus **Add image** (native picker) and drop onto
the brief editor (propagation stopped so the chat composer never takes it).
Save uploads staged images to `item.owner_host_id` (relayed when remote) via
`attachments::upload_attachment`, converts them to saved paths (a retry
after a lost answer resends the identical request), and sends
`BoardItemChanges.attachments` only when the list differs. Upload failure
keeps the edit with "<name> couldn't upload; nothing was saved." Readback:
`edits.brief.images[{saved}|{staged}]`. Assumption: `BoardUpdate` accepts
UI-uploaded paths under the owning host's uploads for the item's project.

Delete row (final tweak): `render_delete` shows the disabled row for any
loaded, non-deleted ticket the engine doesn't offer Delete for (including
Done with live/queued/background work or an unavailable owned chat). Ready
says "Move it to Backlog first…"; every other case says what is still
working and to let it finish or stop it. Done is never told to move to
Backlog.

Rank contract (LUN-11): `board/model.rs` `board_order` compares
`item.rank` as plain strings (fixed-width `r`+15 hex keys, the store's SQL
order), then id; no longer `compare_pin_order`.

## Fixture stages added in checkpoint 3

- `chat-workbench-fixture` w20n, after the card's Edit check: delete the
  card's automation via the reader (agent host path) while its chat is
  shown; the inline card must reach `Deleted` in < 4 s (watch, not the 30 s
  window) → `w20n-lun2-card.json` (per-frame serialization of the card
  store's production `record(id)`: revision, updatedAtMs, deleted,
  lifecycle), `w20n-lun2-card-deleted.png` (painted proof). No test-only
  accessor. The delayed-in-flight-response interleaving is code-reviewed
  (fenced by `read_generation`), not reproduced.
- `board-detail-fixture` (cases 21–23), after stage 7 (`d08-update-sent`),
  before stage 8:
  - 7b `brief-image-thumbnail`, click `detail-brief-image-0` → lightbox
    (`d08b-brief-image-lightbox`), Escape → `lightbox-escape-keeps-the-drawer`;
  - 7c Edit brief → `detail-brief-image-remove-0` → drop a new PNG on
    `detail-brief-input` → Save → `brief-images-replaced-through-the-engine`
    (BoardRead: one new uploads path, revision up), new thumbnail loaded
    (`d08c-…`, `d08d-…`), `running-ticket-not-deletable`;
  - 7d BoardCreate a throwaway ticket → open → `detail-action-delete-ask` →
    `delete-asks-before-sending` (still listed) → `detail-action-delete` →
    `delete-removes-the-ticket` (drawer closed, not listed, unreadable or
    tombstoned) (`d08e-delete-confirm`, `d08f-deleted`).
- Not covered by a UI fixture: badge reconciliation after a *missed*
  journal event (the engine journals every archive/delete, so the gap can't
  be produced from the UI boundary without a test-only seam). The snapshot
  reconciliation is exercised on every f13 badge read.

Build/run for Codex:
```
scripts/cargo.sh check --locked -p luna-ui --tests
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture --example board-detail-fixture
target/debug/examples/board-detail-fixture <ABS_EVIDENCE_DIR>
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture --example board-chat-ui-fixture
target/debug/examples/board-chat-ui-fixture <ABS_EVIDENCE_DIR>
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture --example chat-workbench-fixture
```

## Checkpoint 4 (repair window)

- **Stale editor refusal (confirmed):** `Phase::Error` was only replaced by
  the next Review, so the Once refusal "Enter the time as HH:MM." stayed in
  the footer after switching to Interval (`w20n-automation-editor.png`).
  `Editor.error_basis` now records what the form would send when the
  refusal was shown (`Err(local reason)` or the reviewed/previewed
  request); `editor_phase()` reads an `Error` whose basis no longer equals
  `editor_request()` as `Editing`. Previewing/Reviewed/Applying/Unknown are
  never rewritten. The footer, the Reload button and the readback `phase`
  use it. Coverage: w20n, right after `kind=interval, every=60` following
  the refused review, asserts `phase == "editing"` and the old reason gone.
- **Detail fixture floating menus:** `reveal()` held the review-mode menu's
  option (a window-anchored popup, ending at y 887 in a 900 px window) to
  the scrolling body's bounds and wheel-scrolled a popup that can't scroll.
  `FLOATING_MENUS` + `floating_option()`: an option `{trigger}-{n}` of a
  painted trigger must lie fully inside the window viewport and is pressed
  where it paints (real hit test); everything else keeps the body rule.
  `failure.png` is already written on failure (`capture(...failure.png)`).

Rerun: `board-detail-fixture` (reveal + new 7b–7d stages) and
`chat-workbench-fixture` (w20n stale-refusal assertion).

## Final repair slot (board-detail-fixture) — PASS

Cause: `open_picker` clicked a picker chip at its last painted bounds
without scrolling it into the drawer body; the new brief image row pushed the
review profile's chips below the fold, so the press hit clipped content and
the HarnessModel menu never opened. Fix (fixture only): `reveal()`'s wheel
loop is now `scroll_into_body(locate)`, used by `reveal()` and by
`open_picker` (when a drawer is open) with the chip's painted bounds
(`chip_rect`); presses still hit-test real bounds. The delete confirmation
is revealed before its capture (`d08e`). No assertion was relaxed.

Evidence (`target/qa/board-automation-fixes-2026-10-10/`):
- `claude-detail-build-1.log`, `claude-detail-build-2.log`: exit 0 (only
  pre-existing warnings in untouched files).
- `claude-detail-run-1/` + `.log`: PASS, 55 ok, `board-detail.pass`.
- `claude-detail-run-2/` + `.log` (final, after the d08e capture fix): PASS,
  55 ok, `board-detail.pass`, captures d01–d14 incl. `d08b` lightbox,
  `d08c` image edit (× + Add image), `d08d` replaced thumbnail, `d08e`
  full delete confirmation, `d08f` deleted; `manifest.json`,
  `readback.json`.
- Disk after: `target/debug` 14G; 352 GiB free.

Review: no new `appshots-fixture` items in `crates/ui/src` (readback fields
extend existing QA readbacks); the card accessor was removed. UI diff
+2486/−475 (src 1886/−465, examples 600/−10).

## Progress

- [x] LUN-1 list row (checkpoint 1)
- [x] LUN-1 editor redesign (checkpoint 2)
- [x] LUN-2 UI revalidation + revision floor (checkpoint 1)
- [x] LUN-2 watch subscription (checkpoint 2)
- [x] LUN-12 banner filter + receipt pruning, drawer delete (checkpoint 2)
- [x] LUN-13 drawer thumbnails + lightbox (checkpoint 2)
- [x] Fixture stages w20m LUN-2/LUN-1 and board-chat-ui f13 (Codex runs)
- [x] board-detail-fixture: thumbnail/lightbox, image edit, delete (checkpoint 3)
- [x] Review fixes: card read generation, monotonic freshness floor, badge snapshot races (checkpoint 3)
- [x] LUN-13 brief image add/remove (checkpoint 3)
