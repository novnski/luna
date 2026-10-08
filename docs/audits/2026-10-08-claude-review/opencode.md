# OpenCode adapter audit — 2026-10-08

Baseline: `d3819203`, reviewing OpenCode changes since 2026-10-06. Scope is the
OpenCode harness modules and examples. Builds, runtime QA, commits and app
handoff are coordinated by the main reviewer.

## Failure cases recorded before changes

1. The first `/api/event` HTTP request returns 200 but closes without a body.
   A later request delivers the first SSE frame. `bus_task` increments its
   generation on HTTP admission, so this first live subscription announces
   `Reconnected`, never `Connected`. Both initial turn and compaction admission
   wait for `Connected`; a healthy retry unnecessarily waits 15 seconds, and
   compaction refuses to start. Repeated empty 200 responses also continually
   reset the retry counter, defeating the documented reconnect budget.
2. During reconnect, `readback_frames` recovers a completed text/reasoning item
   before buffered live deltas are processed. Snapshots and deltas share a
   part ID, but no state records that an ended snapshot is final. The queued
   delta appends the recovered suffix a second time; a later full snapshot is
   rejected as a rewrite, leaving duplicated transcript text permanently.
   An old `started` snapshot must not reopen a recovered completed part.
3. A permission/form settles while SSE is disconnected. Card resync closes
   the prompt, but the separate `pending_requests` map still exempts the turn
   from the activity watchdog. Reconnect also inserts requests from unrelated
   sessions into this map. Readback must reconcile removals only for request
   lists successfully read, retain unknown state after failed reads, and
   exclude unrelated owners. Active global forms remain relevant to the run.
4. A background child finishes while its root is parked and SSE is down.
   Reconnect skips message recovery for inactive roots, and synthetic history
   recovery currently drops the completion's `source`, `childID`, and `state`.
   The child therefore remains open forever. Recover parked root history,
   preserve native completion metadata, and settle only an owned matching
   child. Unrelated completions and repeated readback must not settle another
   child or produce duplicate terminal events.

These cases will be exercised through real loopback HTTP/SSE and the existing
`TurnWire` run-session fixture before production code is changed. These are
protocol integration regressions, not paid-provider or installed-binary proof.

## Implemented findings

| Finding | Severity | Changed paths | Result |
| --- | --- | --- | --- |
| Empty HTTP-200 SSE attempts consume the initial generation and reset retry accounting | P2 | `crates/harness/src/opencode/transport.rs` | Generation advances only after nonempty stream bytes; empty attempts remain in the reconnect budget. |
| History readback overtakes buffered text/reasoning deltas | P2 | `crates/harness/src/opencode/wire_v2.rs`, `parts.rs` | Ended snapshots carry a final marker; subsequent starts/deltas cannot reopen or duplicate that item. |
| Stale or foreign requests disable the activity watchdog | P2 | `crates/harness/src/opencode/recovery.rs`, `turn.rs` | Each successfully read native list reconciles its request kind, failed/malformed lists retain unknown state, and only owned or active global requests exempt the turn. |
| Parked roots never recover missed background completion and wake events | P2 | `crates/harness/src/opencode/recovery.rs`, `turn.rs` | Parked recovery skips the prior completed execution, preserves synthetic completion metadata, and restores execution boundaries before recovered wake content. Existing child ownership and settle-once guards remain authoritative. |

## Reviewed feature coverage

| Feature and relevant commits | Paths inspected | Edge cases reviewed |
| --- | --- | --- |
| v2-only stdio boot, API contract, redacted wire diagnostics (`500ee04b`, `8494702f`, `2502482d`) | `mod.rs`, `server.rs`, `transport.rs`, `wire_v2.rs`; contract/module references | Readiness versus HTTP admission, private-server lifetime, auth ownership, bounded startup, stream idle/reconnect accounting. |
| Receipts, native steering, reconnect, stalls (`8b5ccbb4`, `e29ad36e`) | `turn.rs`, `delivery.rs`, `recovery.rs`, `bus.rs`, `parts.rs` | Caller IDs, stale terminals, undelivered steers, stop withdrawal, unknown admission, retry notices, replay ordering, activity exemptions. |
| Permissions, forms, session grants and delivery retries (`6eacea49`, `53e9e77e`, `c7e3d5be`) | `permissions.rs`, `forms.rs`, `server.rs`, `turn.rs` | Own/foreign/global owners, deny versus cancel, rule readback, child containment, stale cards, transport-only retries and ambiguous settlement. |
| Images, typed skills, model readback, usage and compaction (`08c7d2e0`, `7ca962cc`) | `inputs.rs`, `prompt.rs`, `usage.rs`, `catalog.rs` references | File ownership, encoded file URLs, bounded decode, unsupported image input, native files/skills on dispatch, model accounting, compaction admission versus completion. |
| Whole-turn fork and rewind (`c88775e4`) | `history.rs` | Exclusive fork boundary, staged reverts, settled-history copy, cursor bounds, checkpoint identity and readback equality. |
| Tool vocabulary and complete native output (`1c62996b`, `9a160a4a`) | `parts.rs`, `wire_v2.rs`, `tool_output.rs`, `shell_tail.rs` | Scoped call IDs, late settlement, partial errors, media omission metadata, multi-file results, shell cursor overlap and final drain. |
| Subagents, background work and per-child Stop (`7c83d0f5`, `f7423900`, `6a968802`, `3c118279`) | `subagents.rs`, `child_stop.rs`, `turn.rs`, `recovery.rs` | Binding, descendants, foreground/background settlement, cancellation, stale stop receipts, unrelated child completion and parked wake recovery. |
| LUNA tools, catalog discovery, titles and QA profile isolation (`eb64e713`, `34681e5a`, `bb4c722e`, `eba24ff1`) | `mod.rs`, `turn.rs`, `prompt.rs`; related commit history/module contracts | First-turn registration fence and scoped model/input interactions reviewed; no independent live title/skill/tool-discovery proof in this lane. |

Local OpenCode checkout source is a separate baseline from the adapter's
originally documented 2.0.22 contract and the live 2.0.24 binary used below;
current source inspection is not treated as installed-binary verification.

## Verification status

Failure cases were recorded before their production edits. All added tests
use loopback HTTP/SSE with the real adapter path:

- `completed_stream_snapshots_ignore_buffered_deltas`
- `empty_sse_attempt_does_not_consume_initial_connection`
- `empty_sse_attempts_exhaust_reconnect_budget`
- `reconnect_reconciles_pending_request_ownership_and_settlement`
- `reconnect_recovers_parked_child_completion_without_crossing_owners`

Tests live in `crates/harness/src/opencode/tests.rs`. The existing `TurnWire`
fixture now supports controlled disconnects and projected-history overrides.
`rustfmt --edition 2024 --check` and `git diff --check` passed. The coordinator
then verified **137 harness library tests** and **13 OpenCode integration
tests**. Evidence: `target/qa/claude-review-20261008/logs/opencode-live-fixes.log`
and the final integration rerun `opencode-live-fixes-r2.log`. The first
integration run exposed an obsolete expectation for an immediate `Error`
before terminal `Done`; the updated single-terminal-error contract passed
the full 13-test rerun. This reviewer ran no Cargo command.

## Completed native verification

The coordinator ran **19 real-binary Go probes; all exited 0**, using installed
**OpenCode 2.0.24** through the existing isolated paid-profile wrapper. Evidence
is under `target/qa/claude-review-20261008/live-go/`. Full per-probe outcomes,
commands and proof limits are in [opencode-live-go.md](opencode-live-go.md).
Source fixes are committed as `8517ae3353256d21cdb2a5eed35475b83fbb27d2`; the
coordinator reports the commit pushed.

Confirmed live results include approvals, deny/cancel distinction, delayed
questions; exactly one foreground and one background child, both completed;
late child approval and separate wake output; per-child Stop and root
continuation; first/resumed LUNA tool delivery and child containment; native
steering/withdrawal; two injected SSE drops with message readback; native
server death and same-session continuation; whole-turn forks and rewind;
native PNG/JPEG inputs; model/variant readback; **idle and live manual
compaction with two native completion events**; command/skill/title discovery;
and complete 85,816-byte shell output through journal/document retrieval.

Wire counts (deduplicated by native event ID): 36 successful prompt admissions,
42 observed execution starts and 62 observed model-step starts (59 Haiku 5.5,
3 MiMo 2.6 Flash), plus two manual compaction operations and three `/generate`
requests. These are distinct metrics, not billed turns/tokens; intentional
stream drops make the observed native step count a lower bound.

OpenCode 2.0.24's product compatibility status remains **`Untested`**. No
certification matrix was changed. Paid harness/engine results are separate from
the coordinator's new signed GUI profile and its T2/manual UI scenarios.

## Remaining limits

- Recovery remains bounded to six 50-message pages and does not invent a
  boundary when the anchor/prior idle is unavailable.
- A running manual compaction still waits for its native completion event;
  this batch fixes initial subscription admission but does not add history
  reconciliation for a compaction outcome lost after admission.
- Live command discovery refreshed on the next turn; `REFRESHED_IN_RUN false`
  means same-turn refresh was not observed. The subagent run proved two child
  modes, but `native tasks: []` means no background-shell task row was proved.
- Scripted input does not prove native panel interaction or multiple selected
  labels. Full GUI restart/queue recovery, side-chat/edit-earlier flows, usage
  limits, mid-turn image steering, automatic compaction, S8 review/goals, and
  final native UI/app handoff remain with the coordinator's separate QA lane.
- Neither fixtures nor these paid probes establish every model's image
  support, provider billing accuracy, or the full compatibility matrix.
- No engine/UI source, provider configuration, worktree, branch, commit, app
  bundle or user data was changed by this lane.

## Native S9Q follow-up: tripled provider error

Native 2.0.24 S9Q evidence:
`target/qa/claude-review-20261008/live/s9q/opencode/run-20261007T232757Z-Cazu3o/native-frames.jsonl`
lines 12–13 contain one root `session.step.failed` (seq 7), then one root
`session.execution.failed` (seq 8), both for the same HTTP-429 `provider.quota`
failure. `target/qa/claude-review-20261008/s9q-opencode-live-readback.json`
contains three identical `kind:error` parts in the same assistant entry.
There is no foreign-session or replay evidence in this sequence.

Failure cases recorded before the follow-up production edit:

1. A step failure becomes `session.warning` and immediately emits `Error`
   without storing `turn.error`; the following execution failure emits another
   `Error`, stores the same text, then settlement emits `Done.error` again.
2. A direct execution failure without a preceding step failure still emits the
   same terminal reason twice. Failures after partial content must remain
   errored and retain their terminal reason.
3. A failed step followed by a successful later step must remain a successful
   turn with a transient warning, rather than a permanent error part.
4. An externally deleted session after partial output must remain a terminal
   failure with an actionable reason when terminal errors are deferred.

Engine review confirms that `Done.error` must stay populated: live processing
can fall back to earlier `Error` events, but crash recovery reconstructs the
reason from the journaled `Done.error`. The document fold appends each `Error`
and `Done.error` independently. Therefore the adapter owns the correction:
step failure is a transient provider warning, terminal failure is stored and
emitted once on `Done`. No engine mutation is needed.

The follow-up changes `bus.rs` and vocabulary comments in `wire_v2.rs`.
`session.error` now marks the turn failed immediately, so a deleted session
after partial output cannot accidentally settle successfully when its error is
held for `Done`. Two failure-first loopback HTTP/SSE regressions cover the
captured quota sequence, direct failure, partial output, external deletion,
successful step recovery and foreign-session filtering:

- `v2_terminal_failures_emit_one_persistent_error`
- `v2_recovered_step_failure_is_a_transient_warning`

Both new tests passed in the 137-test library run. The final 13-test integration
run also checks terminal failure under the revised contract. Native Go
`s4x-server-kill/summary.json` independently reports one crash error and an empty
`duplicateErrors` list, with all six checks true. The exact free-provider quota
failure was not reproduced again in this successful paid batch. Its upstream
quota remains separate from LUNA's fixed three-part rendering defect.

## Paid Go profile command handoff

See [opencode-live-go.md](opencode-live-go.md) for exact main-checkout build and
probe commands, S1–S8 coverage boundaries, and retained artifact paths. A fresh
metadata-only native 2.0.24 read found 30 enabled Go models; the suggested small
default is `opencode-go/claude-haiku-5-5`, with `opencode-go/mimo-v2.6-flash` for
the image/model-switch leg. Catalog evidence under
`target/qa/claude-review-20261008/live-go/catalog/` passed secret scan (zero
findings) and has `SHA256SUMS`. No model turn, credential read/copy, build, GUI
operation or older-worktree build-cache access occurred during this handoff.
