# Workbench performance verification

## Host projection and RPC

The same prewritten E2E workload and allocator ran three times before and after
changes, without another Cargo build or LUNA GUI. The test source SHA-256 is
`79d0c74ed62f2c1d1f2ced47b2a7235206087fdd456e6e2051bf81821745dd18`.
A later Unix-only compile guard and a separate resume acceptance test leave all
measured macOS test bodies identical; `host-resume-source-equivalence.json`
records both hashes and verifies the remaining source is identical.
It uses a real EngineCore, SessionDoc, SegmentWriter, RunJournal and serialized
RPC client. CPU includes the in-process client; it is not whole-app CPU.

| Median measurement | 1,200 entries before → after | 4,200 entries before → after |
| --- | --- | --- |
| CPU, 100 metadata updates | 1,216.10 → 13.71 ms | 3,847.54 → 12.79 ms |
| CPU, 60 stream updates with hidden writes | 783.20 → 214.42 ms | 2,646.92 → 661.79 ms |
| Cold reset wall time | 66.19 → 62.07 ms | 189.25 → 173.58 ms |
| Warm reset p50 | 40.53 → 41.63 ms | 135.75 → 139.43 ms |

Every candidate reused the same content projection for all 100 metadata changes.
Metadata and streaming phases transferred exactly 15,992 and 13,729 JSON bytes,
as before. All 200 full tool outputs remain in the journal; their lazy fetch
SHA-256 stayed identical after same-id rewrites and rewind. Worst RSS growth
across 20 reopens was 14.55 / 38.67 MiB, and growth after the first five opens
6.80 / 2.84 MiB. All host thresholds fixed before implementation passed. These
measurements use ordinary legacy-shaped requests, so they do not take the
optional confirmed-resume shortcut used by the native UI.

The change retains the existing transcript mirror. A bounded set of at most
128 raw slot indices identifies eligible per-entry edits. Structural events,
continuations, skipped malformed slots, changed identity or uncertain shape
fall back to a complete projection. There is no second raw-history cache.
Unwatched commits discard the mirror and require a fresh attach projection.
Metadata publications keep the same content Arc; RPC still reads current usage,
context and provider notice without diffing unchanged entries.

The prewritten correctness E2E also checks coalesced text/tool/nested-activity
updates, completion, reorder/removal, continuation joins, malformed slots,
context-only changes and replacement while closed against a fresh full read.
The 180 focused correctness/regression checks passed: the new projection E2E,
50 existing engine integration checks, 10 replay/provenance checks and 119
existing document checks. Engine/document all-target Clippy passed with existing
warnings and no warning in the new host measurement or projection paths. A legacy Claude history fixture
still unwrapped an obsolete view lease; it now asserts the current null lease
and exercises its unchanged native fork/rewind/edit scenarios successfully.

Evidence: `target/qa/harness-evolution/slice-6/comparison.json`,
`host-baseline-mimalloc-{1,2,3}/host.json`, `host-candidate-resume-{1,2,3}/host.json`,
and `host-correctness-resume-final/projection-correctness.json`.

## Activity polling

A separate native CPU profile identified repeated whole-history reads in
Activity reconciliation. Its projector only needs the latest assistant entry;
a direct backward reader now skips trailing user/system rows and uses the same
salvage policy for malformed assistant maps. Queue, recovery, completion and
automation sources are unchanged, and no Activity cache was added.

A prewritten real-RPC E2E measured eight GetAttention/Claim pairs over six local
chats. CPU fell from 931.18 to 14.26 ms at 1,200 entries per chat, and from
4,659.69 to 25.70 ms at 4,200 entries: 98.5% / 99.4% lower. It preserved latest
approval selection, trailing non-assistant/malformed values, missing-role
salvage, resolution and replacement, user-only/empty histories, privacy and
notification deduplication. The three existing Activity E2Es, projection E2E
and 119 existing document checks also passed after this change.

Evidence: `attention-baseline`, `attention-candidate`, `attention-comparison.json`
and the diagnostic-only `ui-profile-1200-1/cpu-sample.txt`. The profile run is
excluded from performance comparisons because stack sampling adds overhead.

## Native workbench

### Reopen contention and JSON decoding

The diagnostic reopen profile found Activity bootstrap encoding every envelope
under the shared database connection lock. Preparation now happens before that
lock, bounded to 8,192 rows and 32 MiB of retained buffer capacities. Larger
batches encode the remainder one row at a time in the original atomic
transaction. One prepared SQL statement serves the replacement. The caller's
privacy/policy guard still covers preparation and commit.

The prewritten file-backed acceptance fixture passed exact payload/search
filters, concurrent atomic visibility, duplicate-ID rollback, Appshot subset
replacement, restart, unrelated rows, and both byte/count fallback paths.
Maximum concurrent read latency fell from 130.42 to 13.85 ms at 1,200 events
and from 480.54 to 38.59 ms at 4,200 events. These are four-replacement fixture
measurements, not whole-app responsiveness. Session-history RPC checks and
existing Activity checks passed. Evidence: `activity-index-baseline-final`,
`activity-index-candidate-final`, and the preserved diagnostic profile.

The profile also identified serde's flattened/untagged intermediate buffering
on the UI thread. The additive JSON decoder retains the derived decoder as a
compatibility oracle and leaves the wire format unchanged. All 264 prewritten
JSON cases and seven frame-application scenarios passed, alongside 119 existing
document checks. Five 4,200-entry decodes took about 85 ms each before and 56 ms
after, excluding input cloning; this is diagnostic wall time. Main and child
watches await one background decode at a time and retain their post-await
selection, cancellation, malformed-frame retry and desync handling. Evidence:
`decode-parity/decode-parity.json` and its frozen source.

The decoder-only native runs passed correctness but still missed the fixed
warm-open target (`ui-candidate-decoded-{1200,4200}-1`: 82.50 / 221.08 ms).
They are retained as intermediate results. Main now optionally sends the
existing bounded copy's host token. The engine confirms only an exact immutable
content snapshot; that initial response is an empty delta with fresh context
and historical cutoff metadata. The UI moves the retained entries into its
active transcript only after matching token, chat and count. Every retry
requests a normal reset. No additional history cache is retained.

The failure-first serialized RPC test passed unchanged reuse, metadata updates,
forged/cross-chat tokens, same-length rewrite, immediate closed replacement and
restart. Its 1,200-entry opening payload fell from 1,243,564 to 35,023 bytes.
Legacy requests omit the new field and retain their original reset/delta shape;
an older host's reset still follows the existing UI path. The projection and
resume E2Es, document checks and all five existing RPC replay/context tests pass.
Evidence: `resume-candidate/resume.json`, `host-correctness-resume-final`, and
`logs/luna-perf-replay-resume-verified.log`.

### Matched native acceptance

All 70 fixed host/native comparison criteria pass. The final native cohort is
`ui-candidate-resume-1200-{2,3,4}` and `ui-candidate-resume-4200-{1,2,3}`;
each run passes all 42 functional checks, including 20 host-confirmed reopens.
Every retained measured frame is foreground and complete, and all six runs use
one executable and fixture source. Selection was fixed before the repeats;
`native-candidate-selection.json` records the earlier focus-invalid run and
its exclusion by validity. Intermediate failures and measurements are retained.

| Native median measurement | 1,200 entries before → after | 4,200 entries before → after |
| --- | --- | --- |
| Warm reopen p50 | 87.90 → 38.01 ms | 241.88 → 67.62 ms |
| Cold open | 117.86 → 128.67 ms | 281.26 → 307.12 ms |
| Four-pane process CPU | 2,825.57 → 1,692.93 ms | 6,172.41 → 2,363.12 ms |
| Four-pane input-to-present p95 | 10.89 → 11.55 ms | 16.96 → 11.98 ms |
| Four-pane frame-interval p95 | 12.83 → 14.28 ms | 14.68 → 13.20 ms |
| Reopen RSS growth | 72.69 → 58.61 MiB | 180.61 → 138.91 MiB |
| Matched-phase peak charged footprint | 738.69 → 773.38 MiB | 1,236.66 → 1,314.89 MiB |

Warm reopen improved 56.8% / 72.0%; four-pane CPU fell 40.1% / 61.7%.
Cold opens were about 9.2% slower and charged footprint about 4.7% / 6.3%
higher, within the fixed acceptance limits. Fingerprinting and composer history
scans in the four-pane typing phase both fell to zero. One- and two-pane CPU,
input, frame and memory thresholds also pass in `comparison.json`.

Six baseline runs are preserved in
`ui-baseline-final-{1200,4200}-{1,2,3}`. They use the production Shell in an
actual macOS Metal window and real TCP IPC to an embedded engine, with the
same mimalloc allocator as the primary app. All recorded frames were active.
A signed baseline executable and source snapshot are retained separately.

The baseline consistently failed same-length text and tool paint checks, and
its fork check exposed the stale adjacent source row. Send ownership, model
picker, queued edit and child activity checks passed. See
[slice-6.md](slice-6.md) for baseline metrics and thresholds set before fixes.

The UI now tracks content generations per entry and revisions per document.
Unchanged histories avoid fingerprint and pending-question scans; same-length
text, tool and nested-activity edits invalidate the painted row and Markdown
caches. Hidden views defer derivation and catch up when shown, while an idle
visible pane still paints its next update without input.

One recently closed Main transcript can be reused. Its retained entries are
bounded by 20,000 entries, 80,000 parts and 32 MiB; retained row/tree caches by
10,000 entries, 40,000 rows and 32 MiB. Charges include nested activity and
metadata. Oversize single entries, count limits, byte limits, replacement and
fresh rendering after reopen are covered by the native fixture.

The complete existing production-Shell workbench E2E passed again on the final
implementation in `workbench-resume-final`, including the crowded-tab regression: a closing
tab's shrinking placeholder no longer leaves the active tab scrolled out of
view. All 41 recorded checkpoints, including forks, keyboard routing, background
work and automations, completed. This fixture uses a hidden GPUI window and
in-memory EngineRpc transport.
All 1,185 existing UI library tests pass with the CI's serial test setting, and
UI/engine/document all-target Clippy passes. An earlier parallel test invocation
aborted in macOS keyboard initialization during `native_diff_font_geometry`;
the apparent handoff test passed in isolation, followed by the complete serial
suite. The native performance fixture uses the separate real-window/TCP
path described above.

## Window resizing and compact controls

The later window-resize report has a separate before-fix native reproduction:
`ui-resize-red-1200`. It exercises five pane/sidebar arrangements over seven
actual macOS window sizes each, with 35 screenshots and 70 verified foreground
frames. Two panes exceeded the 516-pixel half-width viewport by 126 pixels;
four panes exceeded it by 770 pixels. With Files open, four panes overflowed
by 490 pixels even at the initial full width. Chats, drafts, focus and the
saved layout stayed intact. Three arrangement checks failed, as expected;
the frozen fixture and durable failed checks are retained.

`ui-resize-candidate-1200-4` passes all seven checks, including the same 35
window states and 70 foreground timing observations. The canvas shares its
available width without an outer horizontal scroll range; its saved split
ratios, chat identities, drafts and focus remain unchanged. Current viewport
width also replaces stale measured width when the surrounding frame changes.
At narrow widths, transcript/composer gutters shrink and compact menus keep
Send, attachment/model/access choices and group actions reachable. Native
clicks open only pane B's model picker and group menu; Escape dismisses them
without changing drafts, models, sends, stops or the saved layout.

The probes explicitly clear old bounds and repaint the owning views before
readback, because GPUI can reuse a view without rerunning its paint probes.
The earlier `ui-resize-candidate-1200-3` passed the functional checks but lost
foreground focus for 19 timing observations; it is retained and excluded from
timing acceptance. The successful rerun used the identical executable.

## Reproduction

Build the existing resource sampler, then run isolated host measurements:

```sh
clang -O2 scripts/macos-resource-stat.c -o target/qa/harness-evolution/slice-6/macos-resource-stat
CARGO_INCREMENTAL=0 LUNA_PERF_EXPECT_REUSE=1 \
  LUNA_PERF_STAT_HELPER="$PWD/target/qa/harness-evolution/slice-6/macos-resource-stat" \
  LUNA_PERF_EVIDENCE="$PWD/target/qa/harness-evolution/slice-6/host-candidate-1" \
  scripts/cargo.sh test --locked -p luna-engine --test workbench_performance_e2e \
  -- --ignored --test-threads=1 --nocapture
```

Use a fresh evidence directory for each repeat. See the
[native fixture guide](slice-6-ui.md) for its workload and timing method.
After all three repeats of both sizes:

```sh
node scripts/compare-workbench-performance.mjs "$PWD/target/qa/harness-evolution/slice-6"
```

The comparison writes `comparison.json` before failing an unmet threshold.
It verifies successful executable/IPC/signature readback, active timing samples,
three unchanged executables per group, exact input counts and durable output
hashes. External CPU/RSS/footprint samples bracket native phases at 200 ms;
they include the embedded engine and streaming thread. Frame timings include
vsync scheduling and are not GPU timings. The native fixture dispatches GPUI
keyboard/IME events internally. Live Codex/Claude acceptance for the preceding
slices is recorded in their separate verification documents.

Timing acceptance covers the eight phases shared with the baseline:
`coldOpen`, `reopen`, `panes1`, `panes2`, `panes4`, `childUpdates`, `rewrites`
and `ownership`. Every recorded measured frame must be foreground and complete.
Candidate reopen cycles now also retain all away/back frame summaries. The
immutable baseline did not retain those individual reopen summaries; all of
its recorded frames were foreground, but individual baseline reopen frames
cannot be verified retroactively.
Later correctness scenarios retain separate foreground observations under
`postMeasurement`; their resize timings are not used in the comparison.
Resize-only runs still require their own frames to be foreground. The fixture
now confirms the exact native window is active before any benchmark clock
starts, retaining `startup-activation.json`; failure aborts the run.

The additional geometry probes used by the later drop/resize assertions are
disabled during matched timings. Original counters and painted-row records
remain enabled. `LUNA_PERF_MEASURE_LAYOUT_PROBES=1` is a diagnostic override,
recorded in the result and rejected by the matched comparison. The rejected
older attempts remain intact, including `ui-candidate-final-1200-2`, whose
298 measured observations were all inactive before the startup guard was added.

## App handoff and retained evidence

The primary `LUNA Dev.app` refreshed in 87 seconds (87.72 seconds including the
wrapper). Its exact bundle executable is running as PID 80439, owns IPC 59777,
and passes `codesign --verify --deep --strict`. `primary-handoff.json` records
the readback. The signed native and workbench fixture executables are preserved
under `final-executables/`, with SHA-256 files, so they can be rerun after build
cleanup. The native cohort contains 2,400 valid matched frame observations and
420 additional foreground resize observations.

Completed fixture profiles were archived, checked, hashed and removed only
after verifying no fixture or open file still needed them. The 104 GiB dev
build directory exceeded the repository's 70 GiB review threshold. With the
only linked worktree and all collaborators idle/frozen, a verbose Cargo dry run
was reviewed: all 175,340 paths were under this checkout's `target/debug`.
The dev-profile-only clean ran under the shared build lock. The refreshed app,
user data, signed evidence and shared mbx action cache remain available; the
app's process, port and deep signature were verified again afterward. Final
accounted build/cache usage is 34.81 GB, with about 380 GiB free disk space.
`dev-clean-review.json` and the cleanup logs retain the exact scope and result.
