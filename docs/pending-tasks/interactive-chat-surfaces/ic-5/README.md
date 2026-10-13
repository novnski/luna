# IC-5: Find in Conversation

Baseline: `8758745a` (main, 13 October 2026). Live status belongs to
`pending_tasks.py`. Read with the package [design](../design.md#conversation-search),
[implementation](../implementation.md#ic-5--selected-conversation-search),
[verification](../verification.md) and the [IC-0 Find feasibility](../ic-0/README.md#find-feasibility-complete-history).

**Verification state.** Written without compiling or running anything. Every
command below is for the orchestrator; nothing here is evidence until it runs.

## Gap

IC-0 proved feasibility only: an example over a synthetic `SessionDoc`
(F1–F12). The app had no Find bar, no projection, no matcher, no highlight
paint and no Cmd+F route for the conversation. Activity/history search
(`SearchCurrentContent`) searches metadata, not message bodies.

Source of truth (unchanged from IC-0): the selected chat's whole history is
resident in `AppState.transcript` (the `WatchDocMessages` reset), and pane or
subagent chats in `sub_transcripts` through the same watch or a frozen
snapshot. `Transcript::rows` holds a row for every entry; the GPUI list only
virtualizes painting. So Find reads complete history from the rows it already
has, and needs no engine or RPC change. The desktop never calls the paged
`ReadSideChat`; the only incomplete state it can show is a history whose
opening replay has not arrived yet, which Find reports as *Partial history*.

## Failure contract (recorded before the code)

| ID | Credible failure | Owner | Pass condition |
| --- | --- | --- | --- |
| C1 | Only mounted rows are searched; the oldest entry is unreachable. | `transcript::find::tests::find_searches_the_complete_conversation_by_displayed_text` | The oldest entry, 240 rows above the viewport, is found, active and revealed (`logical_scroll_top` is its row). |
| C2 | Emphasis or a link label splits a phrase; a link destination, image source or fence info is searchable. | same | `quick brown` across `**quick**` matches; the label matches; `hidden.example` does not. |
| C3 | A match spans two blocks or two messages. | same | `alpha. omega`, `crossmsg-left crossmsg-right` find nothing. |
| C4 | Folding reuses lowercased byte offsets; Kelvin sign, `ẞ`, emoji, CJK, `İ`, final sigma mismatch. | same | Each matched slice of the displayed text equals the expected literal (`KELVIN` with U+212A, `GROẞE`, `Straße`, `İstanbul`, `ΟΔΟΣ`, `🚀 launch`, `検索テスト`). |
| C5 | Reasoning, tool input/output, Mermaid source shown as a diagram, or another chat's message is searchable. | same | All return zero matches. |
| C6 | Overlapping hits; the case toggle is ignored; whitespace matches. | same | `aaa` in `aaaaaa` is 2; `Zulu` is 1 sensitive, 3 insensitive; `   ` reports nothing. |
| C7 | Projection keys or text drift from what the renderer paints, so highlights land on the wrong element. | `markdown::find::tests::projection_names_each_painted_element_with_its_painted_text` | Every projected segment equals the text the real renderer registers under the same element key (headings, task item, nested list, quote, table, code lines, workspace-file lines, image alt). The active highlight paints inside its element. Code ranges split per painted line. |
| C8 | Streamed tokens, prepended history or rollback renumber or lose the active match; an obsolete query's results land. | `…::find_keeps_its_active_match_through_streaming_insertion_and_rollback` | Real `TranscriptFrame::Delta` append, head upsert and remove: identity kept (`1 of 2`, then `2 of 3`); removed match advances to the next survivor; two queued queries yield only the latest. |
| C9 | A collapsed prompt clips its match; Enter/Shift+Enter do not wrap; Escape leaves Find's fold or viewport, drops the composer focus/draft, or undoes the user's own fold/scroll. | `…::find_reveals_folded_matches_and_restores_only_its_own_changes` | Typed query; fold opens and closes as Find moves; wrap both ways; Escape restores the tail-following viewport, refocuses the composer with its draft; after a manual toggle and wheel scroll, closing keeps both. |
| C10 | A not-yet-replayed history reports a complete zero; the late replay is never searched. | `…::find_reports_partial_history_until_the_replay_lands` | `Partial history` before the reset, then `1 of 2` with the first match active. |
| C11 | Cmd+F steals the file editor's, browser's, terminal's or Board's Find; it opens on a hidden chat; a pane chat's Cmd+F searches Main. | Native QA (S05 below) | See the routing table. |
| C13 | Cmd+F from the composer shows the bar but the shell's focus recovery (against the last rendered frame) hands the keyboard back to the composer, so typed text lands in the draft. | `…::find_shortcut_from_the_composer_moves_typing_into_the_bar` | With the shell's real `restore_mounted_focus` and routing predicate, the Find input is focused right after the shortcut, typed text lands in the query (`1 of 1`), the draft is unchanged, Escape returns the keyboard to the composer. |
| C12 | Search blocks GPUI, or the projection cost repeats on every keystroke or stream commit. | Native fixture (P01) | First match, slowest merge and slowest frame recorded against 150 ms / 16 ms on representative history. |

## Delivered design

- **Projection** (`crates/ui/src/markdown/find.rs`): one segment per painted
  text element, keyed by the renderer's own discriminators
  (`quote_child_ix`, `list_child_ix`, `table_cell_ix`, the new shared
  `file_line_ix`, code `ix`). Runs join, so inline styles never split a
  phrase. User rows project the chip-decorated text the bubble paints;
  run-error rows their one-line message. Tool groups, reasoning, question and
  secret-input chips, images, agent/automation cards and Mermaid fences shown
  as diagrams produce nothing.
- **Matching** (`crates/ui/src/transcript/find.rs`): literal and
  non-overlapping. Case-insensitive search folds per character
  (`char::to_lowercase` plus `ß`/`ẞ` → `ss`, `ς` → `σ`, `İ` → `i`) and maps
  folded byte ranges back to whole source characters in one pass.
  Diacritic-insensitive matching and regex stay out of scope.
- **Off-thread, chunked, fenced.** Rows are projected, folded and matched on
  the background executor in pipelined chunks (128 KiB first for latency,
  then 2 MiB or 8,192 rows, so a 10 MiB / 57k-row pass needs about seven
  UI-thread merges). Each chunk is cut from the live rows on the UI thread
  just before it runs, so starting a pass costs one small chunk, never a walk
  over the whole history; the previous query's results are freed on the
  background executor. A query generation fences every chunk; a new query
  drops the running task. The pass starts at the viewport's top row (while
  the view follows the tail, at the first row the last frame painted), so
  the first match found is the first at or after what the reader sees; it
  becomes active and is revealed.
- **Incremental.** Display segments are cached per row id and version and
  reused across queries. After each row sync only rows whose version changed
  are re-matched, removed rows are forgotten, and a coverage change from
  partial to complete restarts the pass.
- **Identity.** The active match is `(row id, element, byte range)`, never a
  list index. It survives streaming, insertion and edits; when its row goes
  it moves to the next surviving match in reading order.
- **Highlights** are paint-only quads under the glyphs (all matches washed,
  the active one stronger with a border). Inline text maps through the link
  presentation's offset map like the selection. Nothing enters the flatten
  cache, row versions, layout or the document.
- **Reveal** scrolls the row below the bar by identity, opens a collapsed
  prompt only when the match may sit past its visible lines, then corrects
  against the painted active rect after layout (tall rows).
- **Close** (Escape, the close button) restores folds Find opened unless the
  user toggled them since, restores the viewport captured at open only when
  Find moved it and the user has not scrolled since, and returns focus to the
  previous element only if focus is still in the bar. Query and case mode are
  remembered per chat in memory; a short single-line selection seeds a new
  query.
- **Routing.** `transcript::FindInConversation` is bound to `mod-f`
  app-wide unless a customizable shortcut owns that chord. A deeper binding
  (the file editor's `Input` Search) or a raw key handler (Board search,
  terminal, browser) still wins, because the chat surfaces route the action
  only from their own focus:

  | Focus | Cmd+F result |
  | --- | --- |
  | Main composer, or nothing focused, on the chat route | Main's Find (shell root handler). |
  | A pane/workbench chat's composer | That chat's Find (`pane-chat-view` handler). |
  | Inside a Find bar | Refocus and select its query (transcript root handler). |
  | File editor | The editor's own Search binding. |
  | Browser, terminal, Board, palette, settings, diff | Propagates to that surface's own handling. |

  The **Edit → Find in Conversation** menu item dispatches the same action.
  Opening claims keyboard focus twice: once in the same update, after the
  shell's deferred focus recovery has run, and once in the frame that mounts
  the bar (the shell dialogs' `focus_pending` pattern); the query is then
  selected unless text was typed in between.

## Criteria mapping

| Criterion | Delivered | Accepted when |
| --- | --- | --- |
| `contract` | Focus routing above; projection and range maps keyed by renderer elements; exclusion of secret, private and app content (C1–C7, C11). | Lib tests pass; S05 native routing passes. |
| `delivery` | Background chunked search over complete rows, generation cancellation, stable identity navigation with wrap, necessary folds only, Partial history (C8–C10). | Lib tests pass; S03/S04/S06 native steps pass. |
| `verification` | GPUI owner tests (C1–C10), native fixture (P01 timings plus S01–S03 PNGs), native QA steps. Artifact summaries integrate at IC-6. | Fixture exits 0 with retained `result.json`, PNGs and `sha256.txt`; QA notes recorded. |
| `handoff` | The commands below. | Orchestrator: exit codes, hashes, queue checkpoint, LUNA Dev refresh and delivery. |

## Files

Owned (`ui/transcript`, `ui/markdown`):

- `crates/ui/src/transcript/find.rs` (new): state, matcher, chunked search,
  navigation, reveal, folds, bar, fixture readback and owner tests.
- `crates/ui/src/markdown/find.rs` (new): projection, highlight paint and its
  renderer-boundary test.
- `crates/ui/src/transcript.rs`: `mod find`, the `find` field, sync and
  scroll hooks, paint wiring for Markdown, user and error rows,
  `row_workspace_root`, `notice_detail`, root action and bar.
- `crates/ui/src/markdown/render.rs`: `RenderOptions::find`; discriminator
  helpers made `pub(super)`; highlights painted for text elements and code
  lines.
- `crates/ui/src/markdown/mod.rs`: `pub mod find`.
- `crates/ui/src/rail.rs` (the transcript's MessageRail `impl Transcript`):
  the tick-to-row map is cached per row generation. Per frame it deep-copied
  the whole transcript, rebuilt every preview and searched the rows linearly
  for every tick (~370 ms a frame at 10,000 entries / 57,346 rows, the C12
  finding below).
- `crates/ui/examples/ic5-conversation-find-fixture.rs` (new).

Shared, additive hunks:

- `crates/ui/src/shell.rs`: `bind_find_keys` in `apply_keymap`; one root
  `on_action` for Main (through `transcript::open_find_from_composer`);
  `restore_mounted_focus` made `pub(crate)` so the focus test drives the real
  recovery.
- `crates/ui/src/shell/pane_chat.rs`: one `on_action` on `pane-chat-view`.
- `crates/ui/src/app_menus.rs`: Edit-menu item.
- `crates/ui/Cargo.toml`: one `[[example]]` entry.

No dependency, engine, RPC, document or `Cargo.lock` change.

## Commands and evidence

Run from the repository root. Keep each exit code in the run log.

```sh
RUN="$PWD/target/qa/interactive-chat-surfaces/ic5-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$RUN"

scripts/cargo.sh check --locked -p luna-ui --tests 2>&1 | tee "$RUN/check.log"; echo "check exit=${PIPESTATUS[0]}"
scripts/cargo.sh check --locked -p luna-ui --features appshots-fixture --examples 2>&1 | tee "$RUN/check-examples.log"; echo "examples exit=${PIPESTATUS[0]}"

# Owner tests (CI runs the whole lib this way on Linux)
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib find -- --test-threads=1 2>&1 | tee "$RUN/tests.log"; echo "tests exit=${PIPESTATUS[0]}"
# Neighbouring owners touched by the paint and notice changes
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib -- markdown:: transcript:: app_menus:: --test-threads=1 2>&1 | tee "$RUN/neighbours.log"; echo "neighbours exit=${PIPESTATUS[0]}"

# Native fixture. P01 timings: release build, default 10,000 entries (~10 MiB).
scripts/cargo.sh build --locked --release -p luna-ui --features appshots-fixture --example ic5-conversation-find-fixture
target/release/examples/ic5-conversation-find-fixture "$RUN/fixture" --step-timeout-secs 120; echo "fixture exit=$?"
# Correctness only, debug build: a smaller history keeps every step short.
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture --example ic5-conversation-find-fixture
target/debug/examples/ic5-conversation-find-fixture "$RUN/fixture-debug" --entries 1000 --step-timeout-secs 180; echo "fixture-debug exit=$?"

scripts/cargo.sh clippy --locked -p luna-ui --tests 2>&1 | tail -n 40
(cd "$RUN" && find . -type f ! -name sha256.txt -print0 | sort -z | xargs -0 shasum -a 256 > sha256.txt)
```

Expected: every exit 0. The fixture's `result.json` lists checks
`S01.oldest-offscreen-found-and-revealed`, `P01.common-query-complete`,
`S02.display-text-and-unicode-mapping`, `S03.stream-keeps-active-identity`
and `S04.close-restores-search-moved-viewport`, plus
`performance.{worstFirstMatchMs, worstMergeMs, worstFrameMsWhileSearching, stepMs}`
and `budget.withinProvisionalBudget` (`measuredAtRepresentativeSize` is true
only for the release run at 10,000 entries). A budget miss is a recorded
finding, not a failed check. Every step has a deadline
(`--step-timeout-secs`, default 120): a missed one adds a failed
`TIMEOUT <step>` check with what it waited for, the run continues and exits
non-zero. While a step waits, a progress line prints every 10 s. The fixture
drives frames itself (draw, then the next-frame callbacks), so the
post-layout reveal correction runs. On an error it still writes the checks
that ran. Retain `result.json`, the six PNGs, the logs and `sha256.txt`;
commit the reviewed `result.json` summary beside this README under
`evidence/`.

After the checks: `scripts/run-macos-dev.sh --refresh`, then verify the
LUNA Dev process, IPC port and `codesign --verify --deep --strict`.

### Native QA in LUNA Dev (S01–S06 by hand)

Use a long existing chat (or the IC-0/IC-5 fixture text pasted into a test
chat) with code blocks, a table, a link, a long multi-line prompt and a
running reply. Record a screenshot per step in `$RUN/native/`.

1. **S05 routing.** Composer focused: Cmd+F opens the bar at the top right
   with the query selected; the composer draft is unchanged. Repeat from a
   workbench/pane chat composer: its own bar opens. With a file tab's editor
   focused, Cmd+F opens the editor's search, not the bar. Board page: Cmd+F
   focuses the ticket search. Terminal and browser tab: no bar appears.
   Edit → Find in Conversation opens the bar from the composer.
2. **S01/S02.** Search a word from the oldest message: the count shows
   `1 of N`, the view jumps to it, the active match has a stronger wash and a
   border. Search a phrase spanning bold text, a link label (found), its URL
   (not found), a word inside a code block (highlighted on its line), a table
   cell. Toggle `Aa`: counts change for mixed case.
3. **S03.** While a reply streams, keep a match active above it: new matches
   in the streamed text raise the total but the active match and its
   position stay. Rewind or edit an earlier message that holds the active
   match: the bar moves to the next match.
4. **S04.** Search a word on line 8+ of a long collapsed prompt: the prompt
   expands; Enter moves on and it collapses again. Enter at the last match
   wraps to the first; Shift+Enter wraps back. Press Escape without
   scrolling: the view returns to where it was and focus returns to the
   composer. Repeat, then scroll and collapse the prompt yourself before
   Escape: both stay as you left them.
5. **S06.** Switch to a long chat and press Cmd+F with a query immediately:
   until its history arrives the status reads `Partial history`, then a real
   count.
6. **Appearance.** Repeat step 2 in light and dark themes; the bar sits below
   the titlebar fade and above the rows; nothing is clipped at narrow widths.

## P01 finding and fix (fix round 1)

The first release run (`ic5-20261013`, M3 Max, 10,000 entries, 57,346 rows)
missed the budget: worst first match 2,107 ms, worst frame while searching
369 ms, merges at most 1.47 ms. The 369 ms frame was not Find: the
transcript's MessageRail rebuilt its tick-to-row map every frame (a deep copy
of the transcript, 5,000 previews, and 5,000 linear row searches). Every
Find merge notifies the view, so each merge cost a ~370 ms frame, and the
merges (and so the first match) queued behind those frames. Fixed by caching
the rail per row generation and mapping ticks in one pass; Find also cuts
chunks lazily, uses larger later chunks, frees old results off-thread and
keeps the status position without materializing every match. The fixture
now records per-phase attribution (`performance.phases`: prepare, first
chunk, first match, background, slowest chunk and merge, reveal frame) and
budgets every frame of a step (`worstStepFrameMs`), not only frames drawn
while searching.

## Limitations and open items

- **Command palette entry** is not added: `ui/palette` belongs to ZI-05. The
  hook is one entry calling `shell.transcript.update(cx, |t, cx| t.open_find(window, cx))`
  (or a pane chat's transcript); add it after ZI-05 lands.
- **Artifact titles and summaries** (IC-3) are not in the projection yet; add
  them as segments of the artifact row once IC-3 provides the row kind, never
  the artifact source.
- **Mermaid** fences are excluded in chat (they paint as diagrams); a source
  revealed with *Show source* is not searchable.
- **Queries are single-line** (the bar is a one-line input), so a query never
  crosses a code line break; matches inside one line of code are found.
- **`İ` folds to `i`** (the Turkic mapping), a deliberate choice so `istanbul`
  finds `İstanbul`; IC-0's F13 observation is settled this way.
- **Viewport restore on close** follows the spec literally: a viewport Find
  moved is restored unless the user scrolled since. If this reads badly in
  QA, the change is confined to `end_find`.
- **Fold estimate.** Whether a match sits inside a collapsed prompt's visible
  five lines is estimated from a deliberately wide glyph advance; it may open
  a fold that was not strictly needed, never leave a match clipped.
- **Reveal correction** for rows taller than the viewport runs on the next
  frames (bounded to three corrections per reveal); the fixture delivers
  those frames explicitly.
- **Subagent tabs** report complete coverage once their replay baseline (or
  any entry) has arrived.
