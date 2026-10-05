# Delegation UX follow-up

Status: implemented and validated, 2026-10-05. See [verification](verification.md)
for native evidence, repeatable checks and scope limits.

This follows the user's live test of agent workspace controls. The supplied
screenshots show a raw child-result envelope, an oversized agent card and the
desired Discord-style chat reference interaction. The final UI was implemented
with Opus 5.5 at Extra effort in the Claude desktop chat; the coordinator owns engine
behavior, integration, verification and delivery. Preserve unrelated work.

## Intended behavior

1. Human child results show the conversation name and result. Internal turn
   identifiers and agent-facing trust guidance stay out of normal UI while
   remaining intact at the provider/context boundary. Previously stored result
   notices receive the same clean presentation.
2. Creating a helper returns after durable creation, without waiting for its
   output. The parent may continue independent work or finish its turn. A
   completion-triggered parent continuation is an explicit option and queues
   behind an active parent turn. Tool guidance must distinguish this from a
   bounded blocking wait. No recurring timer is needed to receive completion.
3. A standalone `#` in the composer opens chat suggestions. Filtering preserves
   three groups in order: this session's children/forks, other chats in this
   project, other projects in the app. Exact IDs are stored with readable
   labels. Existing `@` file/integration behavior, text editing and input
   provenance remain unchanged. A reference is navigation/context, never a
   grant to control another project.
   Compact badges identify independent **Thread**, fresh **New thread**, and
   context-carrying **Side chat** origins. Generic history forks use the same
   Side chat terminology as the existing workbench. Spaceless side chats
   inherit their source's project for grouping and display.
4. Agent activity uses LUNA's existing theme, typography and provider icons in
   a compact layout: title/status, friendly model/reasoning, one-line prompt or
   result preview, elapsed duration, Open chat and Show in split. Long text and
   narrow panes remain usable. A card's timer updates only that card and stops
   when work stops.
5. Session/fork/side-chat tool access survives native context compaction. A
   compact host-owned discovery instruction belongs in the provider's durable
   system/developer channel, with detailed usage retrieved on demand. Do not
   append a full tool manual to every user message or rely on conversation
   summaries remembering tool names. Check T3's actual adapter delivery paths.

## Failure-first acceptance

| Area | Failure to prevent | Evidence |
| --- | --- | --- |
| Result presentation | A legitimate child result exposes UUIDs or trust-envelope boilerplate | Native render of the supplied historical shape and a new result; the result body and title remain visible |
| Trust boundary | Cosmetic cleanup converts child output into user/system authority for a model | Provider continuation and tool/context reads retain provenance and explicit untrusted-content treatment |
| Background work | The creation call waits for a delayed child, or the parent cannot process another message | Controlled engine E2E holds the child, completes independent parent work, then releases the child |
| Completion routing | A report interrupts an active parent, restarts twice, or is lost on restart | Durable continuation queued after the active turn, exactly once; default display-only behavior also checked |
| Trigger | Headings, code, URLs, escaped hashes or existing file/integration mentions open the wrong picker | Composer E2E with cursor edits and existing mention scenarios |
| Grouping | A global title match outranks a matching child/fork, or duplicates appear across groups | Bare-hash and filtered queries with duplicate titles, children, forks, same-project and cross-project chats |
| Identity | Renaming or a duplicate title changes which chat a selected reference means | Exact-ID chip serialization, draft restore, copy/edit and submitted prompt readback |
| Navigation/scope | A reference silently changes projects, loads messages, or authorizes cross-project control | Selection inserts only a reference; explicit link opening is separate; existing tool access checks remain authoritative |
| Catalog lifecycle | Search results leak across engine/profile switches or apply after the query changed | Generation/revision guards and stale result rejection |
| Scale | Each keystroke normalizes/sorts every chat or renders thousands of suggestions | Reproducible 10k/50k corpus benchmark, bounded matches, index reuse and bounded native popup rows |
| Card layout | Long titles/prompts, model IDs or buttons clip in a narrow split | Native screenshots at normal and narrow widths with long content and both provider icons |
| Timer | Completed duration keeps increasing or timer ticks rebuild the whole transcript | Delayed helper run, frozen completed duration after reopen/restart, narrow notification scope |
| Delivery | New behavior only works on fresh chats or data is lost during refresh | Existing and resumed Codex/Claude flows; signed primary app handoff and preserved user data |
| Compaction | A long chat forgets app tools or receives duplicated instruction blocks | Fresh/resumed/forked adapter catalogs and post-compaction discovery; one bounded bootstrap in the system/developer channel, no manual prepended to each user message |

## Initial evidence

A read-only IPC snapshot of the user's exact Random number child and its
parent found one `create_thread` call and no `wait_threads` call. Both are idle;
the child returned 47, `reportResults` is true and `continueParent` is false.
The current creation path returns its durable receipt and queues startup
independently. This does not establish exact historical UI frame timing, so
the controlled long-running-child E2E will verify non-blocking behavior.

## Design direction

Use the existing native technical workspace style. Domain vocabulary: chats,
forks, helpers, results, providers and background work. Reuse LUNA's graphite
surfaces, foreground hierarchy, muted borders, provider brand icons and
semantic running/completed/attention colors. The signature is a compact agent
row that exposes its execution identity and two direct view actions. Avoid a
large raised card, raw protocol identifiers and a full-catalog menu. Preserve
the app's typography and theme rather than introducing another visual system.

## Verification and handoff

Write/extend end-to-end fixtures before production changes. Do not add unit
tests after implementation. Use locked repository build/check/test/format/lint
commands and save repeatable evidence under `target/qa/agent-delegation-ux/`.
Coordinate builds, keep one LUNA GUI app at a time, preserve the primary data
and unrelated files, refresh the primary app and verify process/IPC/signature,
then follow the repository commit-and-push workflow.
