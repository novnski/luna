# Measured workbench performance and composer ownership

## Failure cases, specified before implementation

1. A hidden streaming conversation makes every transcript and composer inspect
   its history, so typing cost grows with the number of mounted panes.
2. A same-id, same-length text or tool rewrite keeps a stale rendered row.
   Rewind, compaction, fork lineage, context boundary or provider binding changes
   reuse a projection that no longer describes the current conversation.
3. Reopening a settled conversation repeats expensive host materialization,
   wire serialization or presentation preparation without a relevant change.
4. Caching settled history retains unbounded entries, full tool outputs, row
   entities or hidden subscriptions after repeated navigation.
5. Bounding a preview destroys durable full output or hides a changed result.
   A large tool result or nested child activity is accidentally copied on every
   ordinary text delta.
6. Keyboard send, model selection, popup actions, attachment insertion or queued
   editing target Main while another pane's composer has focus.
7. A delayed response from a closed or rebound pane mutates the new composer;
   reconnect produces duplicate sends or loses the selected draft.
8. A performance claim measures an offscreen renderer as whole-app CPU, mixes
   build profiles or workload sizes, or compares runs during other Cargo work.
9. A test inspects only the model while the visible row is stale, or reports
   dispatch success without a durable receipt and exact target/message identity.

## Measurement plan

Establish an unchanged baseline before optimizing. Retain workload inputs,
executable identity, process counters, timings and readback as repeatable
artifacts. Exercise at least 1,000 history entries, large tool previews with
separate durable full output, nested child updates, repeated cold and warm
navigation, multiple panes and focused typing while a hidden chat streams.

Measure host CPU and serialized payload independently from native frame and
interaction timing. Record resident/charged memory through repeated navigation.
Use the same profile, workload, geometry and instrumentation for before/after
runs, with no other builds or LUNA GUI processes running. Set concrete acceptance
thresholds from that baseline, before applying changes.

The UI collaborator owns native workbench instrumentation, presentation and
composer fixes. The coordinating agent owns host/RPC measurements and changes,
combined E2E verification, app handoff and delivery. No rendering rewrite or new
cache is justified without measured evidence. Any new cache needs explicit
entry/byte limits and invalidation for all history and binding changes above.

Native two-pane acceptance must record one sent message and its durable command
receipt in the intended conversation, with the other conversation's draft and
transcript unchanged. Correctness scenarios must inspect visible row output as
well as backing state.

## Host baseline and acceptance thresholds

Three uncontended dev-profile runs of `workbench_performance_e2e`, using the
app's existing mimalloc v2 allocator, were captured before production changes.
At 1,200 / 4,200 entries, median host plus serialized in-process RPC CPU for 100
metadata updates was 1,216.10 / 3,847.54 ms; 60 visible stream deltas alongside
hidden updates cost 783.20 / 2,646.93 ms. Metadata reused zero existing content
projections. Payloads were only 15,992 and 13,729 bytes for those two phases.
Cold reset medians were 66.19 / 189.25 ms; 20 full-history reopens had median
40.53 / 135.75 ms latency. RSS grew at most 9.70 / 34.67 MiB, with only
0.59 / 1.31 MiB further growth after the first five cycles. Full 256 KiB tool
results remained in the journal and were fetched lazily.

Set before optimization: reduce metadata CPU by at least 85%, reuse all 100
unchanged content projections, and reduce streaming CPU by at least 50% if a
bounded changed-row path is introduced. Cold and warm-open medians may not
regress more than 20%; total navigation RSS growth must stay below 50 MiB and
growth after the first five opens below 20 MiB. Retain
correct payload contents, exact same-id rewrites, and byte-identical full output.
These are host thresholds; native frame/input thresholds follow its own baseline.

Earlier exploratory runs used system malloc and a shorter post-rewind history.
Those are retained as diagnostics, not the final comparison baseline. With the
full long history, system-malloc RSS grew while live allocations stayed flat;
the app already addresses that allocator retention. Both measured executables
now match the app allocator. Rewind correctness runs after full-history reopen
measurement so truncation cannot improve the navigation score.

Additional changed-row failure cases, before implementation: several entries
changed in one publication; same-length text, tool and nested activity rewrites;
entry insertion, deletion and reordering; continuation joins whose raw slots do
not match displayed rows; malformed slots and salvaged fields; metadata/context
updates without text changes; a whole-document replacement; and changes while a
view is closed. Every case must agree with a fresh complete document projection.
Any changed-slot bookkeeping is capped and falls back to a full projection when
structure or identity is uncertain. It must not retain a second full history.

## Native baseline and acceptance thresholds

Three foreground native runs per size are archived in `ui-baseline-final-*`,
with the immutable baseline executable and source hashes. Every retained frame
timing sample was active and complete. All six runs reproduced stale same-length text and
tool rewrites; the fork check also failed because its adjacent source kept the
stale row. Keyboard send, model picker and queued edit ownership passed.

At 1,200 / 4,200 entries, median cold-open time was 117.86 / 281.26 ms and
warm reopen 87.90 / 241.88 ms. With four panes, typing input-to-present p95
was 10.89 / 16.96 ms and frame-interval p95 12.83 / 14.68 ms. During the
same hidden-stream typing phase, transcript fingerprinting inspected
91,532 / 298,507 entries and composer scans inspected 42,126 / 147,125.
Whole-process CPU for each complete four-pane phase was 2,825.57 / 6,172.41 ms;
it includes the embedded engine and streaming thread. Reopen RSS growth was
72.69 / 180.61 MiB; whole-run peak charged footprint 738.69 / 1,236.66 MiB.
External memory and CPU samples bracket phases at 200 ms granularity.

Set before production changes: all rendered correctness and ownership checks
must pass. Reduce fingerprinted history and composer entry scans during hidden
streaming by at least 90%; reduce complete pane-phase CPU by at least 30%.
Warm reopen is targeted at least 25% faster, with cold open no more than 20%
slower. Median p95 typing and frame intervals for each pane count must remain
below baseline times 1.25 plus 2 ms (allowing display scheduling noise).
Navigation RSS growth and whole-run charged footprint must not regress more
than 20% or 32 MiB, whichever is larger. Any retained presentation cache must
have explicit count and byte limits and verified eviction/invalidation.
Correctness takes precedence over a timing target; an unmet target requires
investigation and must remain visible in the final results.

## Additional measured Activity read cost

After the first native candidate, a separate two-second macOS CPU sample of
its 1,200-entry one-pane phase showed `EngineRpc::reconcile_attention` repeatedly
materializing all six local histories. The Activity projector only consumes the
last assistant entry, plus queue/session state. This profile is diagnostic and
excluded from timing comparisons.

Before changing that read path, specify and exercise: long settled histories
with only the newest assistant's approval relevant; later user/system rows;
unsalvageable trailing values; an assistant map with missing fields that must
use the existing salvage rules; resolution followed by a new request; an empty
or entirely user-owned history; and repeated GetAttention/transition claims that
must neither duplicate notifications nor include private message text. Measure
the same full RPC sequence before and after. Keep all queue, completion,
recovery and automation producers unchanged. Use a direct tail read with the
existing parser rather than a new cached Activity projection.

The pre-change Activity E2E passed every behavior check. Eight poll/claim pairs
over six chats used 931.18 / 4,659.69 ms CPU at 1,200 / 4,200 entries per chat.
Set before the tail-read change: reduce that CPU by at least 90% while preserving
every request, salvage, resolution and deduplication assertion.

## Activity index write contention: failure cases before the change

A native reopen profile (`ui-reopen-profile-4200-1`) found Activity bootstrap
serializing envelopes and preparing an insert for every row while holding the
shared DocsStore connection. The profile is diagnostic only. Before moving
preparation outside that lock, preserve these properties:

- Successful replacement retains every envelope field, search term, scalar
  filter column, ordering key and task boundary after reopening the database.
- A duplicate-ID insertion failure rolls back the whole replacement and
  leaves the previously committed rows intact.
- Concurrent readers see a complete old or new task, never the deleted or
  partially inserted intermediate state; unrelated task rows remain intact.
- Appshot-only replacement preserves transcript rows, and empty replacement
  removes exactly the requested task or Appshot subset.
- Preparing outside the connection lock must not weaken the existing Activity
  privacy/policy guard held by callers across normalization and commit.
- Extra prepared-row memory is bounded to 32 MiB, with the existing insertion
  path retained for the remainder of larger batches; large batches still
  preserve all rows and rollback semantics.

Use a file-backed integration fixture for persistence, concurrent reads and
injected duplicate-ID failure, then the existing session-history RPC E2Es and
the unchanged native reopen benchmark. Keep the original 25% reopen target.

## Transcript JSON decoding: failure cases before the change

The reopen profile also found the UI thread decoding the entire reset through
serde's flattened struct and untagged enum buffers. The prewritten
`transcript_json_decode` integration fixture compares a direct JSON decoder
with the retained derived decoder: Reset-before-Delta precedence, invalid Reset
with valid Delta fallback, missing versus explicit null fields, unknown keys,
invalid entry and part values, context and replay metadata, and append/upsert/
remove application ordering must preserve acceptance and decoded values.
Large 1,200/4,200-entry frames retain every tool reference and nested activity.

Main and child watches may decode sequentially off the UI thread, but must keep
one frame in flight per watch, await before receiving the next frame, and retain
their binding/cancellation and malformed-frame resubscription guards. Diagnostic
counters remain opt-in and record on the UI thread. No wire format changes.
The native reopen fixture additionally retains each away/back frame summary;
the original baseline retained only the other recorded phase summaries, so its
individual reopen-frame foreground state cannot be retroactively established.

## Confirmed transcript resume: failure cases before implementation

The decoder-only native runs still missed the original warm-open threshold.
An optional request can carry the existing bounded UI copy's opaque content
token. Before adding this handshake, a serialized RPC acceptance fixture must
prove: legacy/no-opt-in requests still reset with unchanged payload shape;
unknown, cross-chat, stale or prior-host tokens reset; an exact unchanged token
returns an empty delta with the correct count and fresh context/replay metadata;
same-length rewrites, append/remove/reorder, closed changes and whole-document
replacement cannot reuse it; metadata-only changes may preserve it; subsequent
live frames remain ordered and receive the current token. No new history cache.

The UI must not paint retained history before host confirmation. Only an initial
empty delta with matching chat/token/count may move the existing seed into the
current transcript; reset, mismatch, malformed response or desync must discard
or invalidate the seed and retry without a resume token. Reconnect and older
hosts must heal normally. Keep the one-chat 20,000-entry / 80,000-part / 32 MiB
limit. Native acceptance must verify all 20 reopens used a confirmed seed and
still run the same rewrite, ownership, cache-bound, fork and compaction checks.
