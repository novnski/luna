# Host-owned automations

This is application work on the owning LUNA host. Provider-owned Claude jobs
remain separate observations; a scheduled prompt uses the ordinary task command,
queue, permission, workspace and provider-acceptance boundaries.

## Failure cases, specified before implementation

1. Two consumers claim the same scheduled occurrence; a lost response or engine
   restart creates another message or another conversation.
2. A paused, edited or deleted schedule from an earlier snapshot still starts;
   a pending occurrence outlives its exact schedule revision.
3. The engine crashes after claiming, after queue acceptance, after native
   acceptance, or before recording completion. Unknown delivery must never
   authorize replay. Queue acceptance must not be displayed as native completion.
4. A foreign or offline host appears to have run work locally. Host identity is
   persisted and checked at every mutation and execution boundary.
5. Sleep produces a burst of backlogged prompts, stale work after newer user
   activity, concurrent runs of one automation, or silent missed occurrences.
6. A one-shot time in the past or invalid timezone is accepted without an honest
   preview. A DST gap or fold produces a duplicate, a shifted surprise or a loop.
7. An archived/deleted target, changed checkout, provider or account is revived
   implicitly; a new-conversation schedule copies obsolete permissions.
8. An agent creates or expands persistent work without one-use local approval,
   reuses a prior approval, forges the source run, or treats model text as consent.
9. Pause or cancel is mistaken for stopping an already executing provider. A
   stale cancel targets unrelated work or a later independent turn.
10. Scheduler metadata writes succeed while task effects fail, or vice versa;
    the ledger loses the deterministic command/message identity needed to
    reconcile rather than replay.
11. A PR read failure, incomplete page, rate limit without reset time, or stale
    check for an older head is shown as success or triggers repeated wakes.
12. Same-second comments are lost through timestamp-only cursors. Own replies
    or unchanged state form a feedback loop. A new head inherits old checks.
13. PR URLs or repo metadata select an arbitrary host, executable or checkout;
    prompt bodies from the PR gain execution authority instead of staying data.
14. A watch exceeds its approved wake budget, continues after pause, duplicates
    an event after restart, or cannot show why it woke a conversation.
15. Listing and history grow without bounds, late responses update another
    composer, or reconnect makes an uncertain mutation safe to repeat.

## Implemented policies

- Each record belongs to one local host and has a monotonic revision. Mutations
  use a stable request id and payload hash; repeat calls return saved receipts.
- Occurrences have deterministic identities and one durable claim. A claim is
  reconciled from its task command and provider correlation after restart.
  Uncertain claims are held for review, never automatically resent.
- One active occurrence per automation. Busy targets and excessive lateness are
  explicit skipped outcomes. Sleep coalesces missed recurring slots into at most
  one eligible latest occurrence; no catch-up burst.
- Support a one-shot instant, a fixed interval, and daily/selected-weekday local
  time in an IANA timezone. Preview the next three occurrences. A nonexistent
  local time is skipped; a repeated local time uses the first instant once.
- Pause prevents new delivery; it does not pretend to stop an accepted run.
  Resume calculates the next future slot. Deletion is retained as a tombstone
  with its history. Cancellation is limited to an exact unsent occurrence.
- New chats use deterministic task identities and the approved origin/checkout
  with a current permission ceiling. Existing chats use their ordinary current
  configuration and explicit revision fences.
- Agent mutation tools require a local Allow once card tied to the source run,
  tool call, exact preview and schedule revision. No task-wide allowance.
- PR watches are distinct event producers, using read-only GitHub access,
  head-specific check state, stable comment ids, a saved wake cursor, bounded
  polling/backoff and an approved finite wake budget. Read errors remain Unknown.

The UI offers a selected-conversation view and a paged manager with saved
history. Controls must explain host ownership, next occurrence, missed/held
outcomes, and the difference between queue admission and completion.

Verification used clock-controlled RPC/SQLite/provider scenarios, the native
workbench and real Codex and Claude one-shots in a disposable signed profile,
including restart readback and exact occurrence, command and message identities.
PR producer checks used controlled responses and a read-only live GitHub check.
See the [acceptance evidence](slice-5-verification.md).

Timezone implementation research: [Jiff time zones](https://docs.rs/jiff/latest/jiff/tz/struct.TimeZone.html)
and [ambiguity policies](https://docs.rs/jiff/latest/jiff/tz/enum.Disambiguation.html).
The existing workspace's Chrono usage is preserved; the new timezone service
can use a dedicated library without migrating unrelated date handling.

Before adding the date-entry helpers and preview expiry: a one-shot wall time
must reject both nonexistent and ambiguous input rather than silently choosing
an offset. A saved preview must expire, and crossing its first due time must
require a fresh preview. A lost metadata response may be checked or explicitly
retried with the exact same request id and body; a missing receipt alone does
not prove that the request never arrived.

Agent-tool failure cases before implementation: an unbound provider thread,
replayed provider call, changed payload under one idempotency key, ordinary
RespondApproval/RespondInput, Allow for task, expired preview, and a changed
schedule revision must never grant persistent work. A real local Allow once
card displays the exact definition and next occurrences; its single-use grant
is consumed only by the still-live source run and call. Tool visibility is
limited to automations anchored on that caller's local conversation/project.

Additional admission and fairness scenarios before hardening: forty due PR watches
with a stalled API must not starve an ordinary due prompt; combined status results
must include page two; active reconciliation must reach older rows beyond its
first hundred. A held user queue must prevent scheduled work from jumping it.
Pause while task admission is blocked must prevent delivery. A new-chat occurrence
must retain the approved permission ceiling and deterministic identity, including
across restart. Failure writing a mutation receipt must roll back its definition.
Creating the new chat is itself a fenced side effect; a pause must not allow a
stale claim to create a conversation or mislabel uncertain creation as skipped.

The Activity producer must report unresolved delivery or PR observation failures
without leaking prompts, names, repository text or provider errors into notification
payloads. A paused error remains inspectable; deleting the definition resolves its
Activity projection while preserving the occurrence history. Archived origins must
not be revived, and an expired or stale preview cannot commit metadata.

An invalid or excessively old persisted calendar cursor must pause that definition
with a failed occurrence and let other due work proceed. It must not fail the whole
sweep every two seconds. The calendar walk has a maximum of 3,660 missed slots.

## Persistence and delivery

SQLite migration 18 adds host-local definitions, mutation receipts and occurrence
rows. A definition revision and its mutation receipt commit together. A stable
request id cannot be reused with a different payload. Preview tokens cover the
exact request, origin authority and revision, expire after 60 seconds, and must
still name a future first occurrence. A lost response can be checked by request
id; explicit retries reuse the same metadata request.

The claim transaction advances the due cursor and creates one deterministic
occurrence, command and message identity. Dispatch separately rechecks ownership,
revision, enabled state, workspace identity and pending work under the ordinary
task-command gate. New-chat creation is fenced too. Existing queued work, active
turns and unresolved recovery make the occurrence skip. Permission changes cannot
raise execution above the ceiling captured when the automation was approved.

Queue admission and provider acceptance are separate. A matching live provider
correlation may become Accepted; exact successful native completion becomes
Completed. A crash, lost acceptance or unproven completion remains Unknown and
blocks another occurrence of that automation. There is no automatic resend of an
uncertain occurrence. Editing a schedule does not erase that uncertainty; inspect
the owning conversation before explicitly creating different work.

Pause prevents subsequent delivery; accepted work follows its conversation's Stop
and recovery controls. Resume chooses a future slot and does not replay missed
ones. A spent one-shot needs an edit with a new future time. Cancel is only for an
exact Claimed occurrence and also pauses the definition. Delete keeps a tombstone
and its history. The last 1,000 occurrence rows per definition are retained, with
active/unknown rows exempt from pruning. Mutation receipts and tombstones remain
for the lifetime of the profile so an old request id cannot recreate work. At most
1,000 nondeleted definitions can exist on one host. Read pages are bounded at 100.

Timed prompts and PR reads have independent two-second maintenance loops. PR reads
are bounded to four concurrent observations, a 45-second observation deadline and
ten pages of 100 rows per result category. The first successful read establishes a
baseline; subsequent head, check/status or new non-self remark changes may wake the
conversation. Each admitted wake attempt consumes its finite budget, including an
attempt skipped because prior work is active. Exhaustion requires an explicit edit
and review of a new budget. Failed reads retain the prior cursor and back off up to
24 hours. External text is marked untrusted and stays within the normal task's
permissions. The producer uses read-only `gh api` calls on github.com.

Activity projects generic failures and unknown observations without prompt, name,
repository or provider-error text in notification payloads. Its target opens the
origin conversation, where Automations exposes the detailed history. Deletion
resolves that projection without deleting its ledger.

## Agent tools

`list_automations` returns at most 20 compact summaries anchored on the current
local conversation. `automation_update` binds the owning host and source project
inside the engine. Models cannot supply permission grants, a host identity or an
approved preview. Every mutation requires an exact, expiring local Allow once card
tied to the live provider thread, turn, run, call id and task revision. Ordinary
provider approval responses and Allow for task cannot grant it. The approved
source is rechecked through the synchronous metadata transaction.

## Repeatable verification

`crates/engine/tests/automations_e2e.rs` drives the real RPC, SQLite, task-command
and provider-peer paths with a controlled clock. Set `LUNA_AUTOMATION_EVIDENCE` to
an absolute directory to retain its JSON artifacts. The opt-in live GitHub test
uses `LUNA_AUTOMATION_LIVE_PR=owner/repo#number` and performs no external writes.
The native workbench fixture exercises the editor and production worker, and
`scripts/qa-automations-live.mjs` verifies real Codex/Claude one-shots and restart in
the separately signed LUNA QA Automations profile. Results and delivery status are
recorded in the verification report after those runs complete.
