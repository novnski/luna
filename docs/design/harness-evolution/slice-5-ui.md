# Slice 5 UI: host-owned automations and PR watches

UI owner: the established Claude conversation (`crates/ui` only). Contract:
`crates/proto/src/automation.rs`, `crates/engine/src/rpc/automations.rs`.
Claude-native scheduled prompts stay where they are (the Goal popover's
"Claude schedules", `composer/goals.rs`) and are never listed, paused or
edited here. OpenCode is out of scope.

## Failure scenarios (written before implementation)

Lists and state
1. A run's Accepted state reads as finished, Unknown reads as failed or
   offers Retry, or Claimed/Dispatching/Accepted/Completed/Failed/Unknown/
   Skipped/Cancelled collapse into fewer states.
2. Pause is described as stopping work a provider already accepted.
3. A one-shot whose time passed offers Resume instead of "edit to a new
   future time".
4. A run's scheduled vs claimed time, coalesced count, task, command and
   message ids or failure detail are missing or unlabeled.
5. Claude-native jobs appear in Automations, or host automations in the
   Goal popover.
6. A conversation owned by another host shows local automations, offers
   create/edit, or doesn't name the owning host and that LUNA must be open
   there.
7. Pages: counts or "all" claims come from a loaded page; Load more
   duplicates or skips rows; history loads unbounded.
8. A refresh or late response for another pane, chat, automation, revision
   or engine connection replaces the current view; after a reconnect the
   old list reads as live instead of reloading saved state and receipts.

Editing and mutations
9. Nothing previews before saving; the previewed payload changes before
   Apply (a field edit after preview must invalidate it); Apply sends a
   different request id or definition than the reviewed one.
10. A stale revision is applied, or silently retargeted to the current one.
11. A lost Mutate answer mints a new request id; only the receipt for the
    same request id may resolve it.
12. A one-shot or calendar time is shown or saved without its IANA zone, or
    in the wrong zone; previewed next occurrences differ from what is saved.
13. Interval/calendar/lateness/name/prompt limits accepted locally but
    refused by the host without saying why (or the reverse).
14. "New conversation" targets anything but the real selected conversation
    as origin; "this conversation" targets another pane's chat.
15. A PR watch accepts a budget outside 1..100 or polls outside 1..1440
    minutes, implies LUNA posts GitHub comments, or hides the wake budget
    used and last error.
16. Destructive actions (delete, cancel occurrence) run without naming what
    changes, or Enter/Escape in a dialog reaches the composer or stop path.
17. Long names, prompts, details or many runs overflow opaque cards.
18. Native ids, credentials or provider payloads appear anywhere.

## Plan

- `crates/ui/src/automations.rs`: view state (focused list, app-wide page,
  run history pages), RPC helpers, labels, fences (chat, automation,
  revision, engine generation).
- Right pane **Automations · this conversation** (a new `RightSurface`):
  this conversation's automations (local host only), each with state,
  cadence in its zone, next due, target, owner host, PR-watch cursor, and
  Pause / Resume / Edit / Delete; expandable run history (paged).
- **Settings → Automations**: the same rows app-wide, paged, with the same
  actions.
- **Editor dialog** (create/edit): name, prompt, target (this conversation /
  a new conversation each run from this origin), trigger (one-shot,
  interval, calendar with ISO weekdays, PR watch), IANA zone, missed-run
  tolerance. **Review** calls `PreviewAutomation` with a request id kept for
  the dialog; any edit after review clears the preview. **Apply** sends the
  identical request plus `previewId`. A lost answer shows "unconfirmed" with
  **Check status** (`GetAutomationMutationReceipt`, same request id).
- Pause / Resume / Delete / Cancel occurrence use the same preview → apply
  flow in a small confirmation.
- Refresh: explicit, on open, on focus change and after each mutation;
  reconnect reloads saved state (see contract questions on push updates).

## Decisions (root, before implementation)

1. Time entry uses `luna_engine::automation::{system_timezone, zoned_ms,
   format_in_zone}`; one-shot wall time refuses gaps and ambiguity.
2. Only the visible view re-reads its first page, every 5 s; later pages
   stay until an explicit reload; answers are fenced by scope and engine
   connection. No watch RPC.
3. "Cancel this run and pause the automation" exists only for Claimed runs;
   accepted work uses the ordinary task stop controls.
4. No error-string matching: the host's message is shown with Reload/Edit.
5. A lost Mutate answer reads `GetAutomationMutationReceipt` first; `None`
   stays unconfirmed. An explicit "Send the same change again" resends the
   identical request id; a new id is never minted automatically.
6. A new-conversation target creates one conversation per occurrence, opened
   from run history; the definition stays anchored on its origin.
7. Run history is per automation (always `automationId`); "Show deleted"
   affects definitions only.
8. Local host only; another host's conversation names that host.

Previews carry `expiresAtMs` (60 s). An expired review disables Apply and
asks to Review again; the request payload is otherwise unchanged.

## Implementation

- `crates/ui/src/automations.rs`: `AutomationsPanel` (scope Conversation or
  All), RPC helpers, labels, editor, row actions, run history, QA hooks.
- `crates/ui/src/shell/automations_pane.rs`: mounts the right pane's panel
  (follows the active conversation) and Settings → Automations; routes
  "Open conversation" to the ordinary chat navigation.
- `crates/ui/src/shell.rs`: `RightSurface::Automations` (surface picker card,
  right-pane `+` row, tab title/icon, close releases the panel) and
  `SettingsSection::Automations` (fixture route `settings/automations`).
- Pause/Resume/Delete/Cancel run use a review then Confirm, Escape closes
  editor and confirmations without reaching the composer.

## Fixture (`chat-workbench-fixture`, step w20m, failure cases 62-72)

Real RPC into the in-process engine with its production 2-second automation
worker; the fixture's Mock peer reports `SessionStarted` only for w20m's exact
prompt (as for w20l), so delivery is durably correlated.

The right pane's Automations surface follows Main, so every scope in the step
is a real Main selection (the production mount, never a hook-set scope).
Delivery targets a fresh local chat ("Automation checks", created through
`Mutate createChat`): earlier steps can leave Main with unresolved delivery
work, and the host deliberately skips targets with queued or unresolved
recovery work. The fixture-managed UI chat list mirrors the engine's own rows
for that chat and for the remote one, and drops both after the step.

- 62: a replicated chat row owned by "Office Mini" (imported into the engine
  with that owner), selected as Main, shows no rows, names that host, and New
  automation is refused with that explanation (case 6).
- 63-64: PR watch limits refused locally with their reasons: owner/repository,
  PR number, poll 1..1440, budget 1..100 (cases 13, 15).
- 65: a one-shot entered in Europe/Bucharest wall time; the host preview is
  exactly that instant (case 12).
- 66: an edit after review disables Apply; restoring the exact request makes
  the review current again (case 9).
- 67: a simulated lost Mutate answer stays unconfirmed under the same request
  id; Check status resolves it from the receipt (revision 1) (case 11).
- 68-69: the worker delivers once to the automation chat; history shows
  Completed with its scheduled (= due) and claimed times, 0 coalesced, command
  and message ids; that chat's transcript has the prompt exactly once (cases
  1, 4, 14). Main and Child A get no new send in the command ledger.
- 70: Pause is reviewed and confirmed at revision 2; a spent one-shot offers
  **Edit time**, which opens that exact revision's editor without sending a
  Resume mutation. Saving a reviewed future time re-enables it. Paused future
  one-shots retain Resume (cases 2, 3).
- 71: rapid real Main switches (Main 2, the automation chat, Main 2) never
  show the automation chat's rows in Main 2's view; a reconnect reloads saved
  rows and creates no runs (case 8).
- 72: 51 more definitions via RPC; Settings shows 50 with a next page, Load
  more adds 2 more once (52 unique) (case 7).

Evidence: `w20m-automations-remote-host.png`, `w20m-automation-reviewed.png`, `w20m-automation-unconfirmed.png`,
`w20m-automation-completed.png`, `w20m-automations-settings.png`, and the
`automations` entry in `readback.json` (refusals, due/local time, request and
automation ids, completed run, paused row, runs after reconnect, paged ids).
On a delivery timeout the last history is saved to `w20m-timeout.json`.

Root-run (in this order):

```
rustfmt --edition 2024 --check crates/ui/src/automations.rs crates/ui/src/shell/automations_pane.rs
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture --example chat-workbench-fixture
target/debug/examples/chat-workbench-fixture ABSOLUTE_EVIDENCE_DIR
```

The step adds about 30-90 s (the one-shot is due at the next minute boundary
at least 30 s ahead).
