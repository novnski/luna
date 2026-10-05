# Native workbench measurement, caching and resizing

The final warm-reopen path uses the existing one-chat bounded entry cache only
after host confirmation. Main opts into `transcriptResume` and offers its
`knownTranscriptToken` once; an exact current snapshot returns an empty delta
with `transcriptToken`, current context and a fresh historical cutoff. The UI
checks chat, token, count and empty-delta shape before moving the seed into the
active transcript. Changed history, old hosts, reconnects and rejected resumes
take the ordinary reset path. Native acceptance counts all 20 confirmed reopens
and retains every away/back foreground frame summary. The immutable baseline
did not retain those individual reopen frame summaries; its other recorded
frames were foreground. Final results are in [verification](slice-6-verification.md).

Root failure matrix: [slice-6.md](slice-6.md) (cases 1-9). This document records
the opt-in counters, repeatable native workload, cache and resize changes, and
the before-fix observations that motivated them. Final accepted evidence and
results are in [the verification report](slice-6-verification.md).

## What is measured, and where (no behavior change)

Counters are off unless `LUNA_PERF_COUNTERS=1` (read once, like
`LUNA_FRAME_STATS`). When off, each site costs one cached boolean check. When
on, they record calls, items, bytes and wall time on the UI thread, per scope:
`main:<chat>` for the main transcript/composer, `pane:<chat>` for a workbench
tab or right-pane chat view.

| Metric | Site | Answers |
| --- | --- | --- |
| `state.frame` / `state.frame_text_only` | `AppState::receive_transcript_frame`, `spawn_subagent_watch` | frames per chat; which ones notify every observer |
| `transcript.sync` | `Transcript::sync` (whole call) | per-chat sync count and time |
| `transcript.sync_unchanged` | its early return | syncs that found nothing to do |
| `transcript.entries_walked` | the per-entry loop | history walked per sync |
| `transcript.fingerprint` (bytes = fingerprint input) | `entry_fingerprint` | settled entries hashed per sync |
| `transcript.row_cache_hit` / `_miss`, `transcript.rows_rebuilt`, `transcript.streaming_rebuilt` | `rows_for` | reuse vs rebuild |
| `transcript.splice` | the minimal list splice | rows actually respliced |
| `transcript.render`, `transcript.render_row` | `Transcript::render`, `render_row` | paint-side work per view |
| `composer.observe`, `composer.pending_scan` (items = entries) | `Composer::on_state_changed` | composer fan-out and its history scans |
| `shell.observe`, `shell.render` | `Shell` observer and `render` | shell cadence |
| `pane.render` | `PaneChatView::render` | pane cadence |

Rendered-row readback: while counters are on, `render_row` records, per
scope, the exact `Row` handed to the painter in that frame (row id, version,
kind and its visible text: Markdown block text, user text, tool call/detail,
subagent status/result). This is the paint input, not the model, and the
fixture pairs it with screenshots of the same frame. It is bounded (2,048
rows per scope, newest wins) and cleared between phases.

## Native fixture: `workbench-performance-fixture`

A real macOS window (`gpui_platform::application()`, Metal), production
`Shell`, and an isolated in-process `EngineCore` reached over its real IPC
socket (`serve_ipc` + `EngineHandle::bootstrap`, the app's serialized RPC
path). Mock harness only; temp provider homes; in-memory auth. It drives
itself from inside the app (GPUI event dispatch: `dispatch_keystroke` with
simulated IME for typing, the same path the headless fixtures use) and quits.
It is not external UI automation.

Workload (mirrors root's `workbench_performance_e2e`): `settled-NNNNN` text
entries with the same template, the first 200 carrying a tool part whose
256 KiB full output lives in the host `RunJournal` behind `outputRef`
(preview only in the doc). Tail additions for visible checks: a few tool and
spawn entries near the end. Seeding writes rows before any watch exists;
every later rewrite/rewind/compaction goes through a detached snapshot imported
atomically (root's `replace_atomically`), never row-by-row on a watched doc.
Streaming and child updates use `SegmentWriter::sync`, the production
incremental writer.

Phases (each resets counters, then records timing, counters, readback and a
screenshot; a failed correctness check is recorded and the run continues; with
`LUNA_PERF_MODE=candidate` the run fails at the end if any check failed):

1. `cold-open`: select the large chat; time to first painted frame with rows.
2. `reopen`: 20 Main switches away and back (same chat), per-cycle timing.
3. `panes-1|2|4`: one, two and four groups, each showing a large chat.
4. `hidden-stream-typing-<panes>`: a hidden chat streams (text appends plus
   periodic tool/metadata upserts through `SegmentWriter`) while the focused
   composer receives 60 typed characters; per-keystroke input-to-frame timing.
5. `child-updates`: 20 nested child activity updates on a spawn chip.
6. `rewrite`: same-id same-length text, tool output and child result rewrites;
   rewind (tail removed); compaction (a range replaced by a summary entry);
   fork (a second chat with the same entry ids, different text) shown beside
   the source; binding (Main switched to that fork). Each checks the rendered
   rows, not only the model.
7. `ownership`: two panes; keyboard send in the focused non-Main pane (real
   Enter), its durable command receipt and exact user message in that chat,
   Main's draft/transcript and sends unchanged; model picker shortcut opens
   only that pane's picker; a queued-row edit begins only in that composer.

Timing (real native frame loop): `on_next_frame` callbacks run at the start of
a platform frame, before `draw`; a task spawned from that callback runs after
the frame's draw and present return. Per interaction the fixture records
input-to-frame-start, frame-start-to-presented and input-to-presented. It also
records frame-to-frame intervals while a phase runs. These are upper bounds
that include vsync waits; they are not GPU timings.

Not measured here: host CPU, wire bytes, RSS/footprint (root samples the
fixture process externally), GPU time.

## Plan (files)

1. `crates/ui/src/perf_counters.rs` (new) and `lib.rs`: the opt-in registry.
2. `transcript.rs`, `composer.rs`, `shell.rs`, `shell/pane_chat.rs`,
   `state.rs`: count-only call sites listed above.
3. `queue_steer_e2e.rs`: a fixture hook to begin/read a queued-row edit
   through the production `begin_queue_edit`.
4. `crates/ui/examples/workbench-performance-fixture.rs` (new) and its
   `[[example]]` entry in `crates/ui/Cargo.toml` (`appshots-fixture`).

## Running the baseline (root)

Same dev profile and wrapper as the LUNA Dev handoff, no release graph:

```sh
CARGO_INCREMENTAL=0 scripts/cargo.sh build --locked -p luna-ui \
  --features appshots-fixture --example workbench-performance-fixture
```

Quit every other Luna GUI app first (one GUI at a time), keep the fixture
window frontmost and the display awake (an inactive window is throttled to
~30 fps by GPUI), run no other Cargo work, then for each size and repeat:

```sh
target/debug/examples/workbench-performance-fixture /ABS/OUT/1200-r1 1200
target/debug/examples/workbench-performance-fixture /ABS/OUT/4200-r1 4200
```

`LUNA_PERF_MODE=candidate` (later, same binary inputs and sizes) fails the run
when any correctness check fails. The fixture prints
`workbench-performance-fixture pid=N` on stderr first; sample that PID
externally (`/tmp/luna-stat PID`, footprint) and align samples with
`phase-marks.jsonl` (`{phase, edge: start|end, unixMs}`, also on stderr).

Outputs per run directory:

- `results.json`: per phase timings (`inputToFrameStartMs`,
  `frameStartToPresentedMs`, `inputToPresentedMs`, as p50/p95/max/mean),
  frame-to-frame intervals during typing, counters per scope, reopen cycles,
  child update latencies, rewrite readback (painted text and row projection),
  ownership evidence (receipt id/status/intent, exact sends per chat, Main's
  draft and entry counts), `checks` and `failedChecks`.
- Screenshots of the last presented frame per phase (`01-cold-open.png`,
  `02-reopen.png`, `03-panes-{1,2,4}.png`, `05-child-updates.png`,
  `06-rewrite-{text,tool,childResult}.png`, `06-fork-beside.png`,
  `06-rewind.png`, `07-model-picker-b.png`, `07-queued-edit-b.png`).
- `phase-marks.jsonl`, `workbench-performance.pass` (mode, entries, failed
  check count) or `failure.txt` (an infrastructure failure, not a check).

## Known limits

- Typing uses GPUI's own event dispatch, not OS events.
- `FrameRecorder` keeps a frame callback armed during typing, so GPUI keeps
  requesting frames (draws still happen only when something is dirty).
- Repaint probes call `window.refresh()` so a visible row is re-rendered and
  its painted text can be read; input timings never force a refresh.
- Compaction is checked on the row projection (its range is above the
  viewport); every other rewrite is checked on painted rows.
- Hidden streaming is written by a background thread through
  `SegmentWriter`, so the host's own work shares the process; host CPU is
  root's separate measurement.

## Candidate changes (after the native baseline)

Baseline evidence (4,200 entries, four panes, hidden-stream typing): the
hidden tab's Transcript synced 49 times over its full history (201,888 of
298,507 fingerprints) while never painted; every visible pane re-walked its
history on each non-text frame of the hidden chat (one app-wide revision);
all five composers rescanned their history on each of those notifications.
Same-length text and tool rewrites stayed stale in all six runs (the row
cache, the Markdown tree cache and some row versions were length-keyed).

Changes, all in `crates/ui`:

1. Per-doc revisions (`state.rs`): each pane/subagent doc has its own
   monotonic revision; the main transcript's revision excludes bumps made
   only for other docs. A view re-derives only when its own doc (or its
   echoes, goal marks or replay state) changes.
2. Content generations (`state.rs`, `EntryGenerations`): every frame records
   which entries it upserted/appended/removed; a reset keeps the generation of
   each entry whose content is byte-identical. Row caches key on the
   generation, so settled history is never re-fingerprinted, and any
   same-id rewrite (same length or not) rebuilds. Generations are trusted
   only when recorded at the doc's current revision; anything else (tests,
   local clears) falls back to a fingerprint that now hashes content.
3. Content-keyed caches and versions (`transcript.rs`): the Markdown tree
   cache keys on length plus content hash; user, error and tool-group row
   versions hash content (computed only when a row rebuilds), so a rebuilt
   row always re-splices and its render cache is invalidated.
4. Hidden views defer (`transcript.rs`, `composer.rs`): a pinned-doc
   Transcript (tab, right-pane chat, subagent) or pane composer not painted
   within 500 ms skips its derivation and catches up at its next render.
   The main view and Main's composer always run immediately.
5. Bounded caches: row and tree caches drop entries that left the
   transcript (after the opening replay); the composer's pending-question
   scan is cached per (binding key, content token).
6. Bounded reopen reuse: AppState keeps one outgoing Main chat's entries and
   generations (moved, at most 32 MiB of content); on reopening that chat its
   first reset keeps the generations of byte-identical entries. The Transcript
   keeps that one chat's row and tree caches (at most 10,000 entries). Any
   other switch discards the kept copy. No full tool output is cached.
7. Fixture: compaction is now also proven on painted rows (Main scrolled to
   its first rows after the performance phases, then back to its tail).

Invariants:

- A row is reused only for an entry whose recorded generation (or content
  fingerprint) is unchanged; a frame touching an entry always changes it.
- A reset never keeps a generation for different content.
- Another doc's frame never re-derives a view or rescans a composer.
- At most one retained chat per AppState and per main Transcript; caches
  never outlive their entries after a replay.
- Hidden views never present stale rows: they derive before painting.

Remaining limitations:

- (Resolved) Drops: each workbench pane owns drops on its own root
  (`PaneChatView`), by pointer position, with Main's semantics. The native
  external drop on pane B passed (`ui-candidate-smoke2-1200`); the internal
  workspace-path drag now aims at B's visible area (it previously landed
  behind the Files pane because of the resize overflow, R1).
- Reopen still waits on the host's full reset and its deserialization on
  the UI thread; the UI saves only the row rebuild.
- A visible pane that was idle for over 500 ms syncs on its next frame (one
  frame of latency, by design).

## Cache bounds and post-measurement acceptance (after review)

Both retained caches have count and byte ceilings, charged conservatively:

- Recent transcript (AppState): at most 20,000 entries, 80,000 parts and
  32 MiB charged. An entry is charged its content (`byte_len`), every id,
  ref, subagent tail and serialized subagent activity, plus 256 bytes per
  entry and 128 per part.
- Retained row/tree caches (main Transcript): at most 10,000 entries, 40,000
  rows and 32 MiB charged. A row is charged 256 bytes plus its ids and
  text; Markdown trees three times their source bytes plus 256 per block,
  and tool lists their detail lines, invocation, refs and serialized
  activity, each shared tree or tool list once.
- A cache over any ceiling is dropped, never kept; admissions and
  rejections are counted (`cache.recent_admitted|rejected`,
  `cache.rows_admitted|rejected`, rejection scopes `<chat>:<ceiling>`).

New phases run after ownership; every timed phase is unchanged.

8. `cache-visibility`:
   - a visible pane idle for over 500 ms (window blurred, measured paint
     age) paints an appended entry with no input;
   - a hidden tab changed while hidden does not sync, and paints the change
     when revealed;
   - with ceilings lowered to 1 MiB (fixture-only setters), leaving the
     4,200-entry Main and fork is rejected by bytes, and so is a chat with a
     single 2 MiB child result; with default ceilings one chat is kept,
     the next switch replaces it, and reopening it reuses at least 90% of
     its rows and paints its current tail.
9. `drop`: a real 1x1 PNG is dropped through GPUI platform file-drop events
   at the centre of pane B's painted bounds; it must stage in B's composer
   only, with Main's attachments and draft unchanged (`09-drop-pane-b.png`).
   Expected RED until drop routing is fixed.

## Second candidate round

- Drops: `PaneChatView` owns drops on its own root (not archived panes):
  external files stage in that pane's composer, workspace-path and file-tab
  drags insert a mention there, each stopping propagation; the pointer, not
  keyboard focus, picks the owner. The first candidate showed the RED:
  nothing staged in B or Main (no listener covered pane B).
- Phase 9 adds an internal workspace-path drag: a real file in the project,
  Main's Files surface open, a press on the file row (bounds from an opt-in
  probe, `file-row:<path>`), held-button moves with frames, release at pane
  B's centre; B's draft must mention the file and Main's draft is unchanged.
  It was written before the fix but first runs with it.
- Idle visible pane: a bounded wait (8 s) for a real quiet interval, with
  `frame.request` (every `request_animation_frame` site) and
  `frame.driver` (transcript spring, own-turn, viewport finalize, collapse
  scroll) counters recorded over the wait to name what keeps the window
  painting. If it never goes quiet, the fixture SIMULATES the idle state
  (ages B's paint stamp, disclosed as `simulatedIdle: true`); the check is
  that the update is painted with no input either way.
- Row-cache charge now includes each tool's serialized call, mention paths,
  attachment ids/paths/names/appshot metadata and badge labels and details.
  Phase 8 also proves count ceilings alone reject (entries; parts and rows).
- `state.frame_decode` times transcript frame decoding on the UI thread.

Remaining UI cost seen in the first candidate (4,200 entries, proposals
only, nothing applied): composer observation still costs about 1.1 ms per
call with scans cached (five composers on every app-state notification);
Shell render about 0.65 ms per frame; warm reopen's UI share is about 26 ms
of 209 ms (row sync 10 ms, frame apply 4 ms, shell renders 12 ms), the rest
is the host's reset, transfer and the decode now counted.

## Window resize (user report, failure cases before any fix)

Reported: after a window manager snaps LUNA to half screen and back to full
width, chat groups stretch or get pushed around and the workbench becomes
horizontally scrollable. Wanted: chats stay stable and evenly distributed
in the available width, with no outer horizontal scrolling. This replaces
the earlier overflow-canvas / minimum-pane design. Source evidence:
`workbench/render.rs` `min_tree_size` enforces 320x240 per leaf with a
ratio-derived total, the canvas sets `.min_w/.min_h` inside
`.overflow_scroll()`, and Main uses `geometry.main_width` measured in the
previous frame with no invalidation when the width changes.

- R1. A group's painted bounds extend beyond the workbench viewport
  horizontally (behind the Files pane or off-window).
- R2. The workbench has an outer horizontal scroll range or offset.
- R3. Pane widths do not scale in proportion when the window narrows or
  widens (stretch/push instead of even distribution).
- R4. Main's or a pane's composer pill or transcript stays sized for the
  previous width (stale measured width) or exceeds its pane.
- R5. Returning to the original width does not restore the original
  geometry.
- R6. Resizing changes chats in groups, drafts, keyboard focus, or the
  saved layout (ratios, revision): a view-only change must not persist.
- R7. Short-and-wide windows behave differently from narrow-and-tall ones
  in any of the above.
- R8. Legitimate inner scrolling (code blocks, tab strips) is affected; it
  stays separate from the outer workbench.

Phase 10 (`resize`, post-measurement) reproduces this on the real window
(`Window::resize`, the platform resize path): a fresh Main (the small chat)
with 1, 2 and 4 groups, 4 groups with Files open, and 2 groups with the
left sidebar collapsed; each cycles 1600x1000 -> 800x1000 -> 1600x1000 ->
864x1000 -> 1600x1000 -> 1600x560 -> 1600x1000. Immediate and settled
geometry are recorded (viewport, canvas, `canvasScroll`, every pane's
chats and painted bounds, Main/B composer pills, layout tree, revision);
settled states are checked for R1-R6 and screenshotted
(`10-resize-<variant>-<size>.png`). The window returns to 1600x1000.
Expected RED before the layout fix.

QA support:

- `LUNA_PERF_PHASES=resize` runs only the resize repro on the same seeded
  window and engine; default runs keep every phase, resize last.
- Every timed sample records the window's content size; the measured
  phases must all run at 1600x1000 (a check before phase 8;
  `offSizeSamples` per timing summary). The resize phase is exempt.
- The internal drag targets the centre of pane B's bounds intersected with
  the workbench viewport, and clears earlier file-row probes before opening
  Files; no visible point of B fails with that reason. (The first run's
  target was inside the Files pane because B's bounds overflowed: R1.)

### Fix plan (after the RED run)

RED (`ui-resize-red-1200`, 35 screenshots): groups2 at 800 px had an outer
scroll range of 126 px (viewport 516, panes held at 320); at 864 px 62 px;
groups4 at 800 px 770 px with three groups outside the viewport (about 1.6
groups visible); groups4 with Files open overflowed even at full width
(490 px, viewport 796) and 1,014 px when narrow, with B/D behind Files.
groups1 and groups2 with the sidebar collapsed passed; layout, drafts and
focus stayed stable.

1. `workbench/render.rs`: the content fills the viewport exactly (no
   `min_tree_size` floor); splits use the saved ratio (bounded only by the
   reducer's ratio range) instead of re-clamping each side to 320/240 px.
   The outer scroll container stays only so its handle reads zero; inner
   scrolling (tab strips, code blocks, transcripts) is untouched.
2. Main's width is the measured pane width only while the column width it
   was measured at still holds; otherwise the saved proportions are applied
   to the current width, and a mismatch between used and measured width
   requests one more frame. The Shell's rail uses the same current width.
3. Saved layouts are never written by a resize (ratios, axes, revision).
4. `min_tree_size`/`clamp_ratio` stay for divider-drag and split-creation
   limits and the advisory `canvasMinimum` readback.
5. `chat-workbench-fixture` steps that asserted the old overflow design
   (w15c, w16c/w16d; failure cases 26 and 28) now assert the new one:
   every group inside the viewport, no outer scroll, proportional widths,
   composers inside their panes, the same saved layout.

## Final UI design notes (slice 6)

- Row derivation is per doc and content-keyed: per-doc revisions, recorded
  entry generations (frames), content-hashed parse cache and row versions.
  No full tool output is cached. Hidden pinned views and pane composers
  defer their work to their next render.
- Exactly one recent chat is kept per AppState and per main Transcript,
  under explicit entry/part/row and charged-byte ceilings; oversize chats
  are never kept; reuse requires byte-identical entries.
- Drops are owned by the pane under the pointer.
- The workbench always fits its viewport: groups share the available width
  and height in their saved proportions, with no outer scrolling; resizing
  never writes the saved layout; Main uses its current width. Inner
  scrolling (tab strips, transcripts, code blocks) is unchanged. Divider
  drags and split creation keep their 320x240 limits (advisory
  `canvasMinimum`). Narrow groups get the compact tiers below.
- Opt-in counters (`LUNA_PERF_COUNTERS=1`) and fixture hooks
  (`appshots-fixture`) observe only; root owns the measured results and the
  verification record.

## Narrow groups (failure modes before the fix)

RED evidence from the passing-geometry build: `10-resize-groups4-half800.png`
(about 129 px per group: Send overlaps the model icon, the access control is
clipped past the pill, transcript text about 30 px wide with words broken
every few letters), `10-resize-groups4Files-half800.png` (about 68 px per
group: the draft is effectively invisible, Send and Attach overlap, the
header shows only `+`/split with no active-tab identity), and
`chat-workbench-fixture` w15c: the focused active tab was 70 px wide but only
3 px visible (unhittable) because the strip's controls took the rest.

- N1. A composer control (Send, Attach, model, access, overflow trigger)
  extends outside its pill.
- N2. Composer controls overlap; Send is missing; Attach/model/access are
  neither shown nor reachable from an overflow trigger.
- N3. The text input's usable width is below half the pill (and below
  40 px when the group allows).
- N4. The pill does not fit its group, or a narrow group (< 240 px) keeps
  wide gutters (pill < 80% of the group).
- N5. The transcript text column leaves its group, or a narrow group
  (< 400 px) keeps fixed 48 px gutters (column < 60% of the group).
- N6. The active tab's visible hit target is below min(48 px, 40% of the
  group), or a strip control (`+`, split, close, overflow) leaves the group
  or covers the active tab.

Probes (opt-in, layout-neutral, current frame only): `composer:<scope>:`
`pill|input|send|attach|pickers|model|access|more`,
`transcript:<scope>:column`, `strip:<pane>:split|close|more`, with the
workbench's `plusBounds` and `tabBounds`. Phase 10 checks N1-N6 on every
settled state; screenshots stay the visual record.

### Narrow fix (compact tiers, from the current width)

Every tier is derived from this frame's width; nothing persisted changes
(splits, chats, drafts, focus) and every action stays reachable.

- Composer (`composer.rs`): below 360 px of available width the column
  gutter shrinks to 4% (2-16 px) and the input padding to 6% (6-16 px).
  When the controls row cannot fit two 32 px chips, Attach and the picker
  group move into a **controls tray**: a 24 px More button (`Composer
  controls`, `aria-expanded`) opening an anchored card above the pill with
  the same Attach, model, access, Goal, usage and work controls (each still
  owned by its composer). Send/Stop stays in the row (24 px). A picker opened
  from the tray keeps it open; an outside press closes it; a press on More
  that found it open closes it (no reopen on the same click). Escape, in the
  shell's capture chain, closes an open picker first, then an open tray, and
  never reaches the stop path or the draft.
- Transcript (`transcript.rs`): the 48 px row gutter becomes 10% of the
  measured width (8-48 px) below 480 px, remeasuring the list once when it
  changes.
- Strip (`workbench/render.rs`): a group narrower than 220 px (its saved
  share of the current viewport) shows a compact strip: 2 px padding/gap, a
  40 px tab minimum, and one **Group actions** button (`strip:<pane>:more`)
  in place of `+`, split and close, so the active tab keeps the rest of the
  strip. Group actions opens the tab menu with the group's actions first
  (New thread or side chat..., New thread in a split, Close group: same
  availability as the strip buttons), then the active tab's actions; an
  empty group lists only the group's. `+` bounds are dropped while compact.
  Escape now dismisses the tab menu too. Known limit: the `Creating...`
  spinner that replaces `+` is not shown while compact (the chooser and the
  new tab still are).

### Narrow acceptance (real pointer input, post-measurement)

After the `groups4` resize cycle, phase 10 resizes to 800 x 1000 and, from
painted probes:

1. clicks B's `composer:pane:<B>:more`: B's tray opens, Main's does not;
2. clicks `composer:pane:<B>:model` inside the open tray: B's model picker
   opens, Main's stays closed;
3. Escape closes the picker, a second Escape closes the tray (its controls
   are no longer painted, More still is);
4. clicks B's `strip:<pane>:more`: the Group actions menu is open for B and
   its painted rows (`wb-menu:<row>` probes) include New thread or side
   chat, New thread in a split, Close group, Close tab and the split moves,
   all inside the window; Escape dismisses it.

Throughout, both drafts, both selected models and the layout/revision/group
count are unchanged, no send starts and Escape never reaches the stop path
(escape log). Screenshots: `10-accept-tray-open.png`,
`10-accept-tray-model-picker.png`, `10-accept-tray-dismissed.png`,
`10-accept-group-menu-open.png`, `10-accept-group-menu-dismissed.png`;
readbacks are under `phases.resize.acceptance`. No OS file dialog opens and
no setting changes.

### Probe capture (fresh paints only)

GPUI replays a cached view's previous paint without running its prepaint,
so a probe recorded only when its view re-renders: build 5's settled frames
(about 0.4 ms, nothing re-rendered) read back no probes and every N1-N6 and
acceptance check failed with "not painted". Probes are therefore read as a
capture, never by age: the fixture clears every probe, calls
`fixture_repaint_probes` (notifies the Shell, every visible composer and its
pickers, Main's and each tab's transcript, and refreshes the window), draws
until Main's composer has re-recorded, and reads only what was painted after
the clear (`perf_counters::probes`). Each settled resize state and every
acceptance step (after each click and Escape) is a fresh capture; immediate
states clear before the resize and read what the resize frame painted. A
control that is hidden or moved into a menu is absent, not stale.

The persistent workbench readback also exposes the compact strip:
`groupActionsBounds` (pane -> Group actions, painted while compact; `+`
bounds are dropped then, and Group actions bounds while wide) and
`menuRows` (the open menu's rows by id). `chat-workbench-fixture`'s
`controls_clear` requires exactly one of `+` or Group actions per group,
right of the tab viewport and inside the group.

### Active-tab reveal during width and focus changes

`chat-workbench-fixture` w15c (candidate 3, 980 px window, both sidebars,
after Tab moves focus between narrow groups) left a crowded strip scrolled
past its active tab: the tab was 101 px wide but only 2 px visible while the
next, inactive tab showed. The strip reveal, the canvas reveal and the tab
width cap each settle over a few frames against bounds measured a frame
late and asked for the next step with `cx.notify()` during render; GPUI
only marks the view dirty then and requests no frame, so the remaining
steps waited for an unrelated event and a stale offset stood. They now
notify after the draw (`notify_after_draw`, a deferred notify that
schedules the frame), and the strip's reveal key includes the tab width
cap, so a cap change (compact strip, resize) re-reveals the active tab even
when the viewport kept its width. The user's own strip scrolling between
those changes is still left alone; `tab_center` and its real drag are
unchanged, and the existing w15c failure is the before-fix evidence.

Candidate 4 (same failure with the deferred notify) showed the real gap.
w15b opens the alongside chat first in the second group and closes it. On
the close the reveal acted on the previous frame's bounds (the neighbour
still right of the viewport) and scrolled -100 px; one frame later the
closing tab's ghost still held its place, so the neighbour measured visible
and the reveal settled; the ghost then narrowed to nothing and the active
tab slid 99 px left of the viewport with no key change. The state is
already in `w15c-fitted-canvas.png`, before any click or Tab; GPUI has no
focus-driven autoscroll, so a focused inactive chip cannot scroll a strip.
The reveal now (a) never acts on bounds measured before a key change: it
waits one frame; (b) stays live while a closed tab in that strip is still
narrowing; and (c) re-reveals when that exit ends (part of the key). w15c
now asserts every group's active tab is wholly inside its strip before the
click, and records the readback with the keyboard-focused tab before and
after Tab (`w15cBeforeTab`, `w15cAfterTab`).

### External drop evidence (smoke 3)

The external PNG drop on pane B staged nothing while the internal workspace
drag passed. Before any handler change, phase 9 records each step: whether
a drag was already active before Entered (GPUI installs the file payload
only when none is), a presented frame between Entered, Pending and Submit,
and opt-in counters for the pane/Main drop handler (`drop.external`), what
`add_paths` received and staged (`composer.add_paths`), and a staged file
refused by an in-flight queue edit (`composer.add_staged_blocked`); the
attachment readback adds the composer's failure and `queueEditFinishing`.

Smoke 4's trace: Entered installed the drag (false -> true), Submit ended it
(true -> false), yet no handler counter moved and nothing was staged or
refused. GPUI's drop listener requires `hitbox.is_hovered(window)`, and
`HitboxId::is_hovered` returns false whenever the window's last input was
the keyboard. `Window::dispatch_event` sets that modality from the raw
event before translating `FileDrop` into MouseMove/MouseUp, and `FileDrop`
kept the previous modality, so after any keystroke (phase 9 follows typed
and Escape input) every hitbox read as not hovered for the whole drop:
Main's and every pane's `on_drop::<ExternalPaths>` were unreachable. A real
macOS drag session delivers no mouse-moved events to reset it, so a user who
types and then drags a file in from Finder hits the same thing. The
internal workspace drag passed because it sends real MouseMove/MouseDown.
Fix (vendored GPUI, `vendor/zui/crates/gpui/src/window.rs`, one match arm):
`FileDrop` Entered/Pending/Submit count as mouse input. The fixture keeps
the keyboard-then-drop order (the realistic case) and records the modality
before and after each drop step (`keyboardModalityBefore/After`).

Smoke 5 passed, but its last input before the drop was not a keystroke, so
it did not prove the typed-then-drop case. Phase 9 now sends a real Left
keystroke into B's focused input in the same callback as Entered and
requires keyboard modality after the keystroke, pointer modality after
Entered, B's draft unchanged, one file staged in B, and Main unchanged.

### Layout probes are off while measuring

The layout probes (composer `pill|input|send|attach|pickers|model|access|
more`, picker chips, transcript column, strip `split|close|more`, menu
rows, Files rows) cost a canvas, a name and a map insert per probed element
per paint, which the baseline never paid. They now have their own gate,
`perf_counters::probes_enabled()` (counters on and probes switched on),
default off; the fixture turns them on at the start of phases 9 and 10
(resize-only mode included), and every probe read there is a fresh capture
that re-renders the probed views. Counters, rendered-row records and frame
hooks are unchanged. `LUNA_PERF_MEASURE_LAYOUT_PROBES=1` (diagnostic) keeps
probes on from startup; results record `layoutProbesWhileMeasuring`
(default false). Phase 10 also activates the test window once and waits,
bounded, for the platform to report it active before its timings, recorded
under `phases.resize.activation` and as a check.
