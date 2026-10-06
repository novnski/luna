# Chat timeline (TL) — findings

Source baseline `590cf4b6`. All measurements are LUNA logical px unless noted.
Evidence renders came from the headless fixtures `native-agents-fixture`,
`claude-extensions-fixture`, `chat-mermaid-fixture`, `appearance-fixture`,
`goal-card-fixture` and `side-chat-pane-fixture` (2× PNGs, cropped into
`shots/timeline/`), plus the user's live screenshot of the working indicator.

## Area overview

The timeline already has real craft in places. Streaming text fades in per
chunk with an adaptive 120–400 ms veil (`markdown/veil.rs:34-39`), tool rows
arrive with a staggered 360 ms expo-out reveal and a connector that draws
through its elbow (`transcript.rs:127-133`), the group header chevron rotates,
the stick-to-bottom spring never overshoots, and the activity tree with
rounded elbows is distinctive. Where it stops feeling like an Apple product is
consistency and the moments of change. A tool row's disclosure snaps open
behind a hard clip with an invisible, glyph-swapping chevron. The working
indicator is a sparse flicker of dots beside a random whimsical verb. Errors
use four unrelated visual languages. Text tiers collapse to one grey. Nine
corner radii and two different "code box" components share one column. The
fixes below mostly reuse helpers that already exist (`theme::focus_ring`,
`code_block_frame`, the title shimmer, `CONTROL_RADIUS`/`PANEL_RADIUS`) rather
than inventing new chrome.

---

### TL-24 — Working indicator: sparse flickering dots, random whimsical verb, flat jumping timer (user-requested)
- **Severity:** P1
- **Category:** motion · states · typography · color/contrast
- **Where:** `crates/ui/src/transcript.rs:6759-6902` (`render_working_trailer`; layout `:6855-6900`, spinner call `:6868`), flavour words `transcript.rs:2319-2347` (`FLAVOUR_WORDS`, `FLAVOUR_ROTATE_SECS = 7`, seeded per chat by `flavour_seed` `:2351`), `format_elapsed` `:2374`; glyph `crates/ui/src/loaders.rs:113-147` (`gradient_spinner`), opacity curve `crates/proto/src/motion.rs:72-81` (`gspin_opacity`, `GSPIN_DIM = 0.1`), clock `crates/ui/src/motion.rs:53,113,409` (`PULSE_TICK` 33 ms, `pulse_delta_slow` stride 2 = 15 Hz, `GRADIENT_SPIN` 750 ms). Evidence: `shots/timeline/user-working-indicator-marinating.png`.
- **What's wrong:**
  - **Glyph reads as a static scatter.** It is a 3×3 matrix of 2.5 px dots (10 × 10 px box, 1.25 px gaps), tinted top-to-bottom with the accent glyph palette (≈#A9AFFF / #7C86FF / #615FFF). Each cell spends 47% of the period resting at 10% opacity and is above 50% opacity only 29% of the time. On average only ~2.6 of 9 dots are visible: the screenshot shows two lavender dots and one deep-violet dot, peaking at #7E79B5 (5.0:1). Resting dots composite to ≈#1A1A23, which is 1.1:1 against the #0A0A0B canvas.
  - **Choppy motion.** The glyph runs on the 15 Hz shared loader clock (66 ms ticks), so each 750 ms period gets ~11 frames. The 8% ramp-up lasts 60 ms, less than one tick, so dots jump from 10% to 100% in a single frame and then decay. The eye sees flicker, not a wave travelling upward.
  - **Text lacks hierarchy.** The verb is 12 px `text_muted` (rendered #C9C9CC) followed by a static "…" (U+2026). The timer is 11 px `text_faint`, but faint is hardened to #CCCCCE, so the timer renders *brighter* than the verb (see TL-05). Nothing separates them except an 8 px gap. Digits are proportional.
  - **Hard-cut verb changes.** The verb changes every 7 s with a hard cut. Words run from 6 to 13 characters ("Musing" to "Combobulating"), so the timer jumps sideways by up to ~45 px in one frame.
  - **The verb says nothing about the work.** It comes from 21 whimsical words ("Marinating", "Noodling", "Riffing", …), seeded by chat and unrelated to what the agent is doing ("Marinating…" while `cargo test` runs). The text sits in a `Role::Status` node, and the label changes every 7 s, which may make assistive tech re-announce it.
  - **One idiom for every state.** The same glyph and verb also cover "Sending…" and provider notices; queued sends switch to warning amber with no glyph change.
- **Why it hurts the premium feel:** This is the only thing on screen for the whole duration of every turn, so it is the most-seen motion in the app. A flicker of dim dots next to a random joke word reads as a placeholder, not as a system that knows what it is doing. Apple's equivalents (Siri, Xcode build status, Apple Intelligence) use one calm, continuous motion and plain, specific words.
- **Proposed fix:** Three directions are mocked as filmstrips with resting states. **Recommended: C with A's motion.**
  - **A — shimmer + breathing orb** (`TL-24-after-a.svg`). Replace the matrix with a 6 px accent core plus a 12 px halo (`loaders::orb`). It breathes on a 1.6 s sine (scale 0.85→1.0, halo opacity 0.16→0.34) at the 30 Hz clock, which is smooth for a slow sine. The verb is a single calm word in secondary (#ABABB0) with a white highlight band sweeping every 1.8 s, reusing the strip-clip shimmer in `tool_group_title` (`:1986`) with its own period constant. Show the timer as "· 13s", 11 px tertiary #8E8E93, with tabular figures (`FontFeatures` `tnum`; Geist supports it).
  - **B — arc spinner + morphing verb** (`TL-24-after-b.svg`). A 12 px box holds a 270° 1.5 px accent arc (9 px, matching cap height) turning linearly every 800 ms as a transform. Verb changes fade out over 120 ms (3 px up, 2 px blur) and in over 180 ms. The line width eases over 250 ms with `EASE_OUT_QUINT`, so the tabular timer glides instead of jumping.
  - **C — live status of the actual work** (`TL-24-after-c.svg`, recommended). Derive the label from `self.rows` inside `render_working_trailer`:
    - newest unresolved tool → `tool_chip_content(call)` as a present participle: "Running cargo test", "Reading transcript.rs" (middle-truncate paths)
    - streaming thought → "Thinking"
    - streaming markdown → "Writing"
    - live subagents → "Waiting on 2 agents"
    - send bridge and degraded states keep their current words

    Render the verb in secondary with A's shimmer, the object in primary, and "· 13s" in tertiary tabular digits. Crossfade 180 ms between statuses with B's width easing. On hover, show a quiet "Stop" text action wired to the existing interrupt (`Composer::interrupt_selected`). Reduced motion: static orb, no sweep, instant swaps.
  - **Whimsy:** retire the timed rotation of 21 playful verbs. When nothing specific is known for more than 20 s, fall back to a refined set chosen by state (Working, Reasoning, Writing, Reviewing). If the personality is wanted, offer "Playful status words" as an opt-in setting that rotates no faster than every 20 s and always crossfades.
- **Before:** `mocks/TL/TL-24-before.svg`, `shots/timeline/user-working-indicator-marinating.png`
- **After:** `mocks/TL/TL-24-after.svg` (= option C, recommended); alternatives `mocks/TL/TL-24-after-a.svg`, `mocks/TL/TL-24-after-b.svg`, `mocks/TL/TL-24-after-c.svg`
- **Effort:** M

### TL-01 — Expanding a tool row is a 140 ms height-only wipe; four different fold clocks
- **Severity:** P1
- **Category:** motion
- **Where:** `crates/ui/src/transcript.rs:120` (`TOOL_FOLD = 140 ms, EASE_OUT (0,0,.58,1)`), row height tween `:7866-7880`, detail body mounted at full opacity `:8139-8215`, group body clip `:8238-8262`, `tool_disclosure_progress` `:2870`; `motion.rs:387` (`COLLAPSE` 180 ms), `transcript.rs:1398-1411` (user bubble 220–850 ms, two curves), `:167` (`FOLD_TWEEN_WINDOW` "RESIZE's 200 ms"). Evidence: `shots/timeline/child-transcript-tool-detail.png`.
- **What's wrong:** Clicking a tool row grows its card height from 0 to the analytic detail height (e.g. 111 px for a 4-line output) over 140 ms with CSS ease-out. The output block is mounted at 100% opacity from frame 1 and clipped by `overflow_hidden`, so a hard edge slices through monospace lines at 38% (35 ms) and 68% (70 ms). Nothing fades or settles. The group fold uses the same 140 ms clip. In the same column, a diff-pane file folds in 180 ms, a long user prompt in 220–850 ms with ease-in-out above 500 px, and the fold-tween window assumes 200 ms. Collapse is the same wipe in reverse.
- **Why it hurts the premium feel:** Expanding a tool call is the main interaction in the timeline. On macOS, disclosures settle with a soft spring and the content fades in with it. A 140 ms clip-wipe reads as a snap with tearing text, and four different durations make the app feel assembled rather than designed.
- **Proposed fix:** Add `motion::DISCLOSURE`: a critically damped spring evaluator beside `CubicBezier` (response 300 ms: 47% at 75 ms, 82% at 150 ms, 99% at 300 ms). Use it for the row detail, the group fold, the diff-pane file collapse and the user-bubble fold (keep ease-in-out only above 500 px). The body fades from 0 to 1 over 180 ms with a 6→0 px rise while the height springs. Collapse runs in reverse, with opacity reaching 0 by 60% of the travel. Drive the chevron rotation from the same clock (TL-02). Keep the analytic heights. Reduced motion still snaps.
- **Before:** `mocks/TL/TL-01-before.svg`
- **After:** `mocks/TL/TL-01-after.svg`
- **Effort:** M

### TL-02 — Disclosure chevrons: three sizes, two behaviours, hidden until hover
- **Severity:** P1
- **Category:** iconography · motion · interaction
- **Where:** group header chevron `crates/ui/src/transcript.rs:7979-7995` (14 px `ALT_ARROW_DOWN`, leading, rotated); tool-row trail `:9241-9275` (12 px, glyph swap `ALT_ARROW_RIGHT`↔`ALT_ARROW_DOWN` at `:9259-9264`, `opacity(0.0)` + `group_hover` at `:9248-9250`); spawn-card arrow `:9271-9275` (11 px in an 18 px `ink(0.06)` r5 tile). Evidence: `shots/timeline/hooks-group-failed-rows.png` (no row shows a chevron).
- **What's wrong:** The group header's chevron is 14 px, sits leading and rotates over 140 ms. A tool row's chevron is 12 px and trails directly after the detail text, so its x varies per row. It is fully transparent until the row is hovered, and toggling swaps glyphs instead of rotating. Agent cards use an 11 px arrow inside a filled 18 px tile. In the hooks fixture, seven expandable rows show no expand affordance at all.
- **Why it hurts the premium feel:** You can't discover anything without hovering, and the one control that does animate does it differently from its siblings. Apple lists use one disclosure glyph, always visible, that always rotates.
- **Proposed fix:** One `Disclosure` accessory: the 12 px Solar `alt-arrow-right`, rotated 0→90° on `motion::DISCLOSURE`. On rows, place it in a trailing accessory column right-aligned to the content edge, at 50% opacity at rest and 100% on hover or focus. Give the row an `ink(0.05)` r6 hover wash so the whole row reads as the target. The group header uses the same 12 px glyph. Agent link arrows become 12 px in the same column with no filled tile.
- **Before:** `mocks/TL/TL-02-before.svg`
- **After:** `mocks/TL/TL-02-after.svg`
- **Effort:** S

### TL-03 — Thought text is hard-wrapped at 96 columns, then each line is truncated
- **Severity:** P1
- **Category:** typography · sizing
- **Where:** `crates/ui/src/transcript.rs:428` (`THOUGHT_WRAP_COLS = 96`), `thought_lines` `:436-620`, `detail_body` Thought rows `.truncate()` `:8774-8795`, `more_lines_row` `:8839-8851`. Evidence: `shots/timeline/tool-output-thought-diff.png` ("… 1 more lines").
- **What's wrong:** Reasoning text is pre-wrapped at 96 characters (≈600–615 px of 12 px Geist) regardless of width, and each pre-wrapped line is then a single-line `truncate()` row. In a 420 px side pane the thought column is ~280 px, so about 55% of every line disappears behind "…". In a 1200 px column, lines stop at ~615 px and 45% of the width stays empty. The tail row also reads "… 1 more lines".
- **Why it hurts the premium feel:** The thinking block is the most-read secondary text in an agent app. It is the only prose in the timeline that can't reflow, so in narrow panes it is effectively unreadable.
- **Proposed fix:** Keep inline runs in `ToolDetail::Thought` and wrap at render time with the text system at the live width (column − 56 px). Cache the wrapped line count per width so `detail_height_for` stays analytic. Use 13/20 sans in secondary. Cap at 6 visible lines with a 20 px fade and a "Show N more lines" ghost button, pluralised correctly; fix `more_lines_row` the same way.
- **Before:** `mocks/TL/TL-03-before.svg`, `shots/timeline/tool-output-thought-diff.png`
- **After:** `mocks/TL/TL-03-after.svg`
- **Effort:** M

### TL-04 — Failed tool rows and agent cards paint everything red
- **Severity:** P1
- **Category:** color/contrast · states
- **Where:** `crates/ui/src/transcript.rs:9030-9035` (`tint = theme.danger` for the label), `:9108-9114` (detail in danger), `:9384-9388` (`activity_rail` glyph tint); agent card the same path. Evidence: `shots/timeline/hooks-group-failed-rows.png`, `shots/timeline/child-transcript-tool-detail.png`.
- **What's wrong:** When a tool fails, its glyph, label and detail all render in danger (#F87171 rendered). In the hooks fixture, 4 of 6 rows are wall-to-wall red, and the failed agent card's "Agent" label, title and status are all red. The header already says "· 4 failed" and is deliberately kept quiet (`:7966-7970`).
- **Why it hurts the premium feel:** Expected outcomes, like a project hook blocking a command, become the loudest thing on screen, and the red text is harder to read than the neutral rows. Apple marks failure with a small red glyph and keeps the text in label colours.
- **Proposed fix:** Keep the label and detail in row tones. Swap the rail glyph to Solar `close-circle` at 14 px in danger, and add a trailing 11 px "Failed" in danger. In the header, tint only the "N failed" segment. Agent cards keep a neutral title and show status as a 6 px danger dot plus "Failed" in 11 px medium (shared with TL-22).
- **Before:** `mocks/TL/TL-04-before.svg`, `shots/timeline/hooks-group-failed-rows.png`
- **After:** `mocks/TL/TL-04-after.svg`
- **Effort:** S

### TL-05 — Text tiers collapse: muted and faint render as the same grey (faint is brighter)
- **Severity:** P1
- **Category:** color/contrast · typography
- **Where:** `crates/ui/src/theme.rs:1944-1953` (`harden_composited_foregrounds`), authored values `:1606-1607`; consumers throughout `transcript.rs`. Measured on `native-agents-fixture` PNG (`shots/timeline/child-transcript-tool-detail.png`).
- **What's wrong:** Authored Luna Dark tones are text #E5E5E5, muted #A1A1A1 and faint #737373. After hardening against the adverse frosted backdrop, the production Shell renders muted as **#C9C9CC** and faint as **#CCCCCE**. Group headers, row labels, row details (#C7C7C9), model names, status words, "Show full output", output lines and the timer all paint at 12.1–12.5:1, against prose at #E8E8EA (16.4:1). Three tiers have become two, and the "faintest" one is the brightest of the secondary tones.
- **Why it hurts the premium feel:** Without a tertiary tone, metadata competes with content and the timeline looks flat and busy. Apple's label / secondary / tertiary ladder is what makes dense lists calm.
- **Proposed fix:** (Token change owned by DS, applied here.) Floor tertiary at 4.5:1 against the composited transcript canvas (≈#080808), not the adverse-desktop composite. Keep a luminance step of at least 1.4:1 between tiers: primary #E8E8EA (16.4:1), secondary #ABABB0 (8.8:1), tertiary #8E8E93 (6.1:1). Then map timeline roles: labels and headers to secondary; model, status, timestamps, hints and the timer to tertiary.
- **Before:** `mocks/TL/TL-05-before.svg`
- **After:** `mocks/TL/TL-05-after.svg`
- **Effort:** M

### TL-06 — Tool output and code blocks are two different components
- **Severity:** P1
- **Category:** consistency · typography
- **Where:** tool detail `crates/ui/src/transcript.rs:8692-8775` (r7 at `:8746`, 26 px header `:782`, 10 px label `:8712`, text "Copy" `:8718-8735`, `text_faint` lines + `truncate()` `:8768`, 12 px mono via `tool_code_size` `:2103`); markdown fence `crates/ui/src/markdown/render.rs:2046-2064` (`code_block_frame` r10), header 28 px `:42`/`:2015`, icon copy `:1957-2000`. Evidence: `shots/timeline/tool-output-thought-diff.png`, `shots/timeline/mermaid-dark.png`.
- **What's wrong:** In the same column the two boxes differ in nearly every detail:

  | | Tool output | Code block |
  |---|---|---|
  | Radius | 7 | 10 |
  | Header | 26 px, 10 px label | 28 px, 11 px label |
  | Copy | text link "Copy" with instant hover | 22 px icon button with 150 ms fade and tooltip |
  | Fill | ink .045 | ink .035 |
  | Edge | hairline .08 | theme.border |
  | Mono | 12 px | 12.5 px |
  | Long lines | each line ellipsised (`truncate()`) | scroll |

  The ellipsis means an error location like `crates/ui/src/transcript.rs:52:5` can be cut and can't be read or selected in place.
- **Why it hurts the premium feel:** Two near-identical surfaces that disagree on radius, type and controls read as two products glued together. Clipping the one line the user needs is a functional regression too.
- **Proposed fix:** Extract `code_block_frame` and `code_block_header` into `render::code_surface(label, actions, body)` and use it for Output, Invocation and Diff (TL-12): r10, 28 px header, 11 px medium secondary label, the `code_icon_action` copy with ✓ Copied, theme mono size 12.5/18, primary text (stderr in `danger_muted`). Scroll long lines horizontally with a 28 px edge fade instead of ellipsising them.
- **Before:** `mocks/TL/TL-06-before.svg`
- **After:** `mocks/TL/TL-06-after.svg`
- **Effort:** M

### TL-07 — Four left edges inside one reply; the group title sits 4 px off the glyph column
- **Severity:** P1
- **Category:** alignment
- **Where:** `crates/ui/src/transcript.rs:7979-7983` (22 px chevron box + 6 px gap → title at x = 28), `:105-113` (`ACTIVITY_GUTTER_WIDTH 48`, `ACTIVITY_ICON_LEFT 32`, `ACTIVITY_TEXT_GAP 8`). Evidence: `shots/timeline/child-transcript-tool-detail.png`, `shots/timeline/hooks-group-failed-rows.png`.
- **What's wrong:** Within one assistant message, content starts at x = 0 (prose, agent cards), 28 (group header title), 32 (rail glyphs) and 56 (row labels, detail panels, "Show full output"). The header title is 4 px left of the glyph column it introduces, which is visible in the crop where "Ran 1 command" starts left of the row icons.
- **Why it hurts the premium feel:** Near-miss alignment is the clearest sign of un-designed UI; Apple layouts snap every element to a small set of columns.
- **Proposed fix:** Make the chevron box 26 px so the header title starts on the glyph column (x = 32), with the chevron still centred on the trunk at 12.5. Keep 56 as the single content column for labels, panels and the fetch button. Prose and cards stay at 0.
- **Before:** `mocks/TL/TL-07-before.svg`
- **After:** `mocks/TL/TL-07-after.svg`
- **Effort:** S

### TL-08 — Error and retry states use four unrelated treatments
- **Severity:** P1
- **Category:** states · consistency · color/contrast
- **Where:** `error_chip` `crates/ui/src/transcript.rs:8467-8525` (r8 `theme.surface_raised` card, 13 px muted `DANGER_TRIANGLE` `:8492`, 11 px "Run error", 20 px r5 copy with no feedback); recovery trailer `:6629-6757` (red 12 px headline + `trailer_action` text links `:9790-9812`); undelivered `:6787-6806` (red link text "Not delivered — click to retry"); failed rows `:9030` (TL-04).
- **What's wrong:** The most serious state, a run error, is a neutral grey card with a grey triangle and no danger colour. Retry and recovery are bare coloured text with 4 px-padded text-link buttons. An undelivered send is a red sentence that doubles as a button. Failed tools are whole red rows. There is no shared glyph, container, radius or button style, and the error copy button gives no "Copied" feedback.
- **Why it hurts the premium feel:** Errors are when users most need calm, legible guidance. Inconsistent treatments make every failure look like a different bug.
- **Proposed fix:** Add `transcript_notice(kind, title, detail, actions)`:
  - r10 container with an 8% fill and 24% edge in danger or warning (≈#1C0F10 / #431E1F for danger on the canvas)
  - 14 px glyph: `danger-triangle` for errors, `refresh` for retrying
  - 13 px semibold title and 12 px secondary detail
  - 24 px r6 buttons, with a solid primary ("Retry", "Send now") and outlined secondary ("Copy", "Cancel")

  Use it for the run error, recovery and undelivered states (compact single-line variant). Failed tool rows keep TL-04's glyph and word.
- **Before:** `mocks/TL/TL-08-before.svg`
- **After:** `mocks/TL/TL-08-after.svg`
- **Effort:** M

### TL-09 — Keyboard focus is invisible on tool rows and inconsistent elsewhere
- **Severity:** P1
- **Category:** interaction · states
- **Where:** group header `crates/ui/src/transcript.rs:7951-7956` and chip header `:8106-8113` (`tab_index(0)`, no `focus_visible`); `trailer_action` `:9809` (`surface_raised` bg, ≈1.3:1); attachment thumbs `:6183-6185`, `:6310-6312` (`border_2`); code actions `markdown/render.rs:1922` (accent wash); subagent card `:9580-9608` (no role, no tab stop). Shared helper `crates/ui/src/theme.rs:2382` (`focus_ring`), used at 36 call sites app-wide and nowhere in `transcript.rs` or `markdown/render.rs`.
- **What's wrong:**
  - Tabbing onto a tool group header or a tool row changes nothing visually.
  - Trailer actions brighten to #1E1E1E on #080808, which is barely visible.
  - Attachment thumbnails gain a 2 px border, growing them 4 px and shifting the strip; `theme.rs:2373-2378` documents exactly this bug.
  - The subagent card can't be reached by keyboard at all.
  - The user-bubble expander has `Role::Button` but no `tab_index` (`:6094-6103`).
- **Why it hurts the premium feel:** macOS users expect the system focus ring on every control. Invisible focus is both an accessibility failure and an unfinished-feeling detail.
- **Proposed fix:** Apply `.focus_visible(|s| theme::focus_ring(s, theme.focus_ring()))` everywhere in the transcript: header, rows, subagent cards (add `Role::Button` and `tab_index(0)`), meta-strip buttons, expander, fetch button, notice buttons and thumbnails, replacing `border_2`. The result is a 2 px inset accent ring (#7C86FF, floored at 3:1) at the element's radius with zero layout shift.
- **Before:** `mocks/TL/TL-09-before.svg`
- **After:** `mocks/TL/TL-09-after.svg`
- **Effort:** S

### TL-10 — Nine corner radii in one timeline
- **Severity:** P2
- **Category:** consistency
- **Where:** literals in `crates/ui/src/transcript.rs`:
  - 4: `:9140` file well
  - 5: `:8499`, `:9058`, `:9124`, `:9252`
  - 6: `:6298`, `:8557`
  - 7: `:6459`, `:8746`
  - 8: `:6423`, `:8476`
  - 9: `:8100`, `:9501`, `:9588`
  - 10: `:8547`

  In `transcript/agent_cards.rs`: 4 `:195`, 5 `:323`, `:446`, 9 `:492`. In `markdown/render.rs`: 4.5 `:932`, 5 `:1973`, 6 `:1935`, 10 `:2059`. Tokens: `crates/ui/src/theme.rs:1302-1306`.
- **What's wrong:** The values 4, 4.5, 5, 6, 7, 8, 9, 10 and 16 all appear, but tokens exist only for 6 (`CONTROL_RADIUS`), 10 (`PANEL_RADIUS`) and 16 (`BUBBLE_RADIUS`). Adjacent surfaces disagree: an r7 output block sits under an r9 agent card next to an r10 code block.
- **Why it hurts the premium feel:** Mismatched corner curvature is visible at a glance on stacked cards and reads as accidental.
- **Proposed fix:** Add `Theme::INLINE_RADIUS = 4` and use four steps:
  - 4 for inline code and file wells
  - 6 (CONTROL) for tiles, badges, buttons and copy controls
  - 10 (PANEL) for code, output, diff, agent card, input chip and notice
  - 16 (BUBBLE) for the user bubble

  Replace every literal in the three files with the matching token.
- **Before:** `mocks/TL/TL-10-before.svg`
- **After:** `mocks/TL/TL-10-after.svg`
- **Effort:** S

### TL-11 — Stacked Invocation and Output boxes form a double hairline; "Show full output" is bare text
- **Severity:** P2
- **Category:** spacing · interaction · states
- **Where:** `crates/ui/src/transcript.rs:799` (`DETAIL_SEPARATOR = 1.0`), panel assembly `:8148-8183`, fetch affordance `:8184-8210` (`text_faint` 12 px, `.hover(text_muted)`), loading label `:7775`. Evidence: `shots/timeline/child-transcript-tool-detail.png`.
- **What's wrong:** Invocation and Output render as two bordered r7 boxes 1 px apart. Their borders read as one thick double rule (visible at 2× in the crop). "Show full output (8 B)" is faint 12 px text with no icon, shape, hover fill or focus style, and its loading state is the text "Loading full output…" alone.
- **Why it hurts the premium feel:** Double borders and link-styled actions look like layout leftovers rather than a designed card.
- **Proposed fix:** Render invocation and output as segments of one TL-06 `code_surface`: "Command" / "Output · 8 B" headers, each with an icon copy, separated by a single hairline. Make the fetch affordance a 24 px r6 ghost button (`ink(0.06)`) with a 12 px Solar `arrow-down`, the label "Show full output", a hover fade and a focus ring. While loading it shows the 2×3 item spinner (TL-13).
- **Before:** `mocks/TL/TL-11-before.svg`
- **After:** `mocks/TL/TL-11-after.svg`
- **Effort:** S

### TL-12 — Inline diffs in the activity tree are frameless and square
- **Severity:** P2
- **Category:** consistency
- **Where:** `crates/ui/src/transcript.rs:8636-8650` (`ToolDetail::Diff` → `changes::render_file_body_with_syntax`, no radius or border), card chrome only `when(!collapses)` `:8099-8104`. Evidence: `shots/timeline/tool-output-thought-diff.png`.
- **What's wrong:** In the activity tree (the normal case), an expanded edit shows hunk headers and red/green washes edge to edge with square corners and no file header or copy action. One row up, an Output block is a bordered rounded box.
- **Why it hurts the premium feel:** Neighbouring results of the same kind look like they come from different apps; the frameless diff also bleeds into the rail area visually.
- **Proposed fix:** Wrap the diff in `code_surface`: header with the file name in 11 px medium, "+N −N" in diff colours and a copy icon; body clipped to r10 so the washes stay inside the frame.
- **Before:** `mocks/TL/TL-12-before.svg`
- **After:** `mocks/TL/TL-12-after.svg`
- **Effort:** S

### TL-13 — Four loader idioms, and running tool rows show none
- **Severity:** P2
- **Category:** states · motion
- **Where:** working trailer `crates/ui/src/transcript.rs:6868` (3×3 `gradient_spinner`); running subagent `:9230` (2×3 `mini_glyph_spinner`); active group title shimmer `:1986-2075` (3.4 s); blob fetch `:7775` (text only); `chip_header_row` `running` limited to subagents `:9018-9019`.
- **What's wrong:** The timeline uses four "working" signals: a 3×3 matrix, a 2×3 snake, a text shimmer and plain text. There is no rule for which means what. An unresolved ordinary tool row (a 40-second `cargo test`) is pixel-identical to a finished one; only the group header shimmer hints that something is live.
- **Why it hurts the premium feel:** Live state is the core information in an agent timeline. Not showing which step is running, while inventing several spinners elsewhere, feels both noisy and uninformative.
- **Proposed fix:** One family: the turn-level indicator (redesigned in TL-24) means "the turn is working"; the 2×3 snake means "this item is working"; shimmer means "this summary is live". While `!tool.resolved` and the run is live, the rail glyph slot shows the 2×3 spinner, then crossfades over 150 ms to the tool glyph, with elapsed time in tertiary. The subagent card puts the same spinner in its status slot. The fetch button uses it while loading.
- **Before:** `mocks/TL/TL-13-before.svg`
- **After:** `mocks/TL/TL-13-after.svg`
- **Effort:** S

### TL-14 — Hover metadata strip mixes text and icon buttons with uneven feedback
- **Severity:** P2
- **Category:** interaction · consistency · color/contrast
- **Where:** `crates/ui/src/transcript.rs:7200-7450`:
  - timestamp `text_muted.opacity(0.55)` `:7207`
  - copy `hover_blend` `:7212-7229`
  - "Side chat" text button `:7284-7302`
  - fork `:7326-7344` and edit `:7356-7376`, with no hover, aria label or tab stop
  - "Rewind" text button `:7400-7415`
  - strip `fade_quick` `:7447`
- **What's wrong:** One strip interleaves 11 px text buttons ("Side chat", "Rewind", instant `.hover`) with 14 px icon buttons. Only Copy fades its hover, at 150 ms; Fork and Edit have no hover state, tooltip, aria label or tab stop. The timestamp composites to #5C5C5C on #080808, which is 3.0:1 for 12 px text. The strip fades in over 150 ms but vanishes instantly when the pointer leaves.
- **Why it hurts the premium feel:** Per-message actions are touched constantly. Mixed control languages and dead buttons with no hover feedback feel unfinished.
- **Proposed fix:** Use a tertiary timestamp (#8E8E93, 6.1:1) and five 24 px r6 icon buttons with 14 px Solar glyphs: `chat-round-line` (Side chat), `git-branch` (Fork), `pen` (Edit), `restart` (Rewind), `copy`. All get `hover_blend` at 150 ms, a tooltip, an aria label, `tab_index` and a focus ring. The strip fades in over 150 ms and out over 120 ms, opacity only, and keeps its lane.
- **Before:** `mocks/TL/TL-14-before.svg`
- **After:** `mocks/TL/TL-14-after.svg`
- **Effort:** S

### TL-15 — Seven chrome text sizes, two below the macOS floor
- **Severity:** P2
- **Category:** typography
- **Where:**
  - 10 px: `crates/ui/src/transcript.rs:8712` (detail header "Output" / "Copy" / "Invocation")
  - 10.5 px: `markdown/render.rs:1985` ("Copied"), `transcript/agent_cards.rs:197` (type badge)
  - 11 px: statuses, model, code language, meta text buttons, "Run error", recovery detail
  - 12 px: tool rows, timestamps
  - 12.5 px: code
  - 13 px: jump pill (`shell.rs:12820`)
  - 14 px: body

  `crates/ui/src/typography.rs` has no named styles.
- **What's wrong:** The 10 and 10.5 px sizes sit below the 11 pt minimum macOS uses for secondary labels. Mono is 12 px in tool output but 12.5 px in fences. Every call site passes its own `ui_rems(n)`.
- **Why it hurts the premium feel:** Tiny, near-duplicate sizes blur hierarchy and look cramped on Retina displays; Apple ships a short named ramp.
- **Proposed fix:** Add `typography::TextStyle { Caption 11/14 medium, Footnote 12/16, Mono 12.5/18, Callout 13/18, Body 14/22 }` and a `.text_style()` helper. Move the timeline to it and retire 10 and 10.5 px. Headings keep `heading_metrics` (TL-19).
- **Before:** `mocks/TL/TL-15-before.svg`
- **After:** `mocks/TL/TL-15-after.svg`
- **Effort:** M

### TL-16 — Icon sizes drift from 11 to 16 px and glyphs collide in meaning
- **Severity:** P2
- **Category:** iconography
- **Where:** `crates/ui/src/transcript.rs:113` (`ACTIVITY_ICON_SIZE = 16` beside 12 px text), `:7990` (14 px chevron), `:8492` (13 px), `:9069` and `:9265` (12 px), `:9275` (11 px); `tool_icon_path` `:8590-8611` (`WIDGET` for Hook/MCP/Unknown/ComputerUse, `CHAT_ROUND_LINE` for Thought and the Question chip `:8563`, `DOCUMENT` for Read and ApplyPatch). Evidence: `shots/timeline/hooks-group-failed-rows.png`.
- **What's wrong:**
  - **Sizes:** six icon sizes (11, 12, 13, 14, 16 px plus 12-in-18 tiles) sit beside 11–12 px text. The 16 px Solar rail glyph has ~13 px of live area, about 1.55× the 12 px cap height, so icons outweigh their labels.
  - **Meanings:** the generic four-squares glyph stands for hooks, MCP tools, unknown tools and computer use; a chat bubble means both "thinking" and "the agent asked a question".
- **Why it hurts the premium feel:** SF Symbols are scaled to the adjacent text and each symbol has one meaning; drifting sizes and reused glyphs make the rows look noisy and less scannable.
- **Proposed fix:** Use a 12 / 14 px ladder: 12 px for chevrons, link arrows, header copy and tile glyphs; 14 px for rail glyphs, meta buttons and notice glyphs, centred on the 18 px line. Reassign glyphs (Solar Linear via `@solar-icons/cli`): Hook → `bolt` (exists), MCP → `plug-circle`, Thought → `lightbulb-minimalistic`, ApplyPatch → `pen-new-square` (exists).
- **Before:** `mocks/TL/TL-16-before.svg`
- **After:** `mocks/TL/TL-16-after.svg`
- **Effort:** S

### TL-17 — Jump to bottom: text arrow, no exit, 1.2 s creeping glide
- **Severity:** P2
- **Category:** motion · iconography · interaction
- **Where:** `crates/ui/src/shell.rs:12746-12840` (`render_jump_to_bottom`, `jump_pill`; "↓" text `:12820`, `dialog_in` entrance only `:12837`, no role or aria); `crates/ui/src/transcript.rs:4585-4637` (`jump_to_bottom` / `engage_pin`), spring constants `:182-195`.
- **What's wrong:** The pill's arrow is a Unicode "↓" text glyph rather than the icon set (an `arrow-down` icon exists). It enters with a 180 ms fade but disappears in the same frame as the click. The glide uses the stick-to-bottom spring (damping .7, stiffness .05, mass 1.25). Simulated, it covers 50% in 133 ms and 90% in 383 ms, then spends ~600–900 ms crawling the last 10%, settling at 1.0–1.27 s. The pill has no `Role::Button`, aria label or tab stop.
- **Why it hurts the premium feel:** A long creeping tail feels floaty rather than decisive; iOS scroll-to-top lands in ~300 ms. A text arrow next to vector icons looks off-brand.
- **Proposed fix:** Use the Solar `arrow-down` icon at 14 px in secondary. On click the pill exits with a 120 ms fade and 4 px drop (`menu_out` pattern). User-initiated jumps of 2.5 viewports or less glide over 320 ms on `EASE_OUT_QUINT` (longer jumps keep the existing teleport-then-glide), then hand back to the spring for streaming growth. Add `Role::Button`, the aria label "Scroll to bottom" and a focus ring.
- **Before:** `mocks/TL/TL-17-before.svg`
- **After:** `mocks/TL/TL-17-after.svg`
- **Effort:** S

### TL-18 — Mermaid diagrams: box-in-box, cool-grey square nodes, source-to-diagram snap
- **Severity:** P2
- **Category:** color/contrast · consistency · motion
- **Where:** `crates/ui/src/markdown/mermaid.rs:40-90` (`background: theme.surface`, node `primary_color` / `border` mapping), `crates/ui/src/transcript/diagrams.rs:342-369` (pending falls through to source / "unavailable"), `markdown/render.rs:1873-1900` (`show_source` swaps the whole block). Evidence: `shots/timeline/mermaid-dark.png`.
- **What's wrong:**
  - **Inner rectangle:** the diagram paints its own #0D0D0D background inside the #0F0F0F code frame, a visible box in a box.
  - **Off-palette nodes:** nodes render #343438 (blue-tinted, unlike the achromatic UI) with a bright #858585 1 px edge and ~2 px corners, against the r6–r10 rounded system.
  - **Height jump:** while the background render is pending the block shows raw source, then swaps to the diagram in one frame, with a height change and no crossfade.
- **Why it hurts the premium feel:** Diagrams are showcase content. Default-looking Mermaid boxes and a jumping row undercut the polish of the surrounding text.
- **Proposed fix:** Make the diagram background transparent so the frame fill shows through. Style nodes with `ink(.06)` over the frame, a `hairline(.10)` edge, r8 corners and 13 px text. Edges get tertiary 1.25 px strokes with open arrowheads, and the accent is reserved for highlighted nodes. While pending, reserve the estimated diagram height with a skeleton shimmer and the caption "Rendering diagram…", then crossfade in over 180 ms.
- **Before:** `mocks/TL/TL-18-before.svg`, `shots/timeline/mermaid-dark.png`
- **After:** `mocks/TL/TL-18-after.svg`
- **Effort:** M

### TL-19 — Markdown headings get the same 12 px gap as paragraphs
- **Severity:** P2
- **Category:** typography · spacing
- **Where:** `crates/ui/src/markdown/render.rs:30` (`MD_BLOCK_GAP = 12`), `:440` (uniform `gap`), `:712-719` (`heading_metrics`: H1 19/27, H2 16/24, H3 15/22); `transcript.rs:1914-1931` (`top_gap_for` uses `MD_BLOCK_GAP` between split rows).
- **What's wrong:** Every block, heading or not, is separated by 12 px. A heading sits as close to the paragraph above as to the one it introduces. H2 is only 2 px larger than 14 px body text.
- **Why it hurts the premium feel:** Without proximity grouping, long answers read as an undifferentiated stream; Apple's text styles pair larger top margins with headline weight.
- **Proposed fix:** Add space above headings only (H1 +28, H2 +24, H3 +20) with 6–8 px below, in both `render_tree` and `top_gap_for`. Set H2 to 17/24 and all headings to semibold (600).
- **Before:** `mocks/TL/TL-19-before.svg`
- **After:** `mocks/TL/TL-19-after.svg`
- **Effort:** S

### TL-20 — Turn rhythm: an empty 32 px lane and a 16 px turn gap split one reply into two
- **Severity:** P2
- **Category:** spacing
- **Where:** `crates/ui/src/transcript.rs:7433` (reserved lane `SPACE_SM + SPACE_MD*2` = 32 px under every settled entry), `:1914-1917` (`turn_start` → `SPACE_LG` 16). Evidence: `shots/timeline/child-transcript-tool-detail.png` ("Two watchers launched." → "Background agents finished." = 70 px), `shots/timeline/turn-rhythm-dark.png`.
- **What's wrong:** Every settled entry reserves a 32 px metadata lane that is empty unless hovered, plus the 16 px turn gap: 48 px between entries before line leading. That holds whether the speaker changes or the same agent posts a follow-up entry (a background completion, a continuation). Inside a reply, blocks are 12 px apart. In the fixture, two consecutive assistant lines sit 70 px apart and read as separate turns.
- **Why it hurts the premium feel:** Messages-style rhythm groups same-sender runs tightly and separates speakers; constant large gaps fragment conversation and waste vertical space.
- **Proposed fix:** Speaker changes get a 28 px turn gap that contains the 24 px meta lane, positioned absolutely and animating opacity only. Consecutive same-role entries use `MD_BLOCK_GAP` (12 px) and show the lane only after the last of the run.
- **Before:** `mocks/TL/TL-20-before.svg`, `shots/timeline/turn-rhythm-dark.png`
- **After:** `mocks/TL/TL-20-after.svg`
- **Effort:** M

### TL-21 — Long prompt collapse: an ASCII "..." line and a glyph-swap toggle
- **Severity:** P2
- **Category:** states · typography · interaction
- **Where:** `crates/ui/src/transcript.rs:6036` (`ellipsis = div().h(line_height).child("...")`), `:6077-6135` (`render_user_expander`: 14 px "Show more", `ALT_ARROW_DOWN`↔`ALT_ARROW_UP` swap, `Role::Button` without `tab_index`).
- **What's wrong:** A collapsed prompt shows five lines, then a sixth 22 px line containing three ASCII periods, then a 14 px "Show more" link whose chevron swaps glyphs. The toggle can't be reached by Tab.
- **Why it hurts the premium feel:** An ASCII ellipsis on its own line looks like a placeholder; Apple truncates with a fade or a proper "…" and a focusable control.
- **Proposed fix:** Drop the extra line and fade the fifth line with a 28 px gradient mask to the bubble fill. Make the toggle "Show more" in 13 px medium secondary, with a 12 px chevron rotating 180° on `motion::DISCLOSURE`, plus `tab_index(0)` and a focus ring.
- **Before:** `mocks/TL/TL-21-before.svg`
- **After:** `mocks/TL/TL-21-after.svg`
- **Effort:** S

### TL-22 — One agent, three status vocabularies
- **Severity:** P2
- **Category:** consistency · states
- **Where:** `crates/ui/src/transcript.rs:8910-8921` (`child_status_label`: "Done", "Interrupted"…), chip status rendering `:9211-9224` (bare 11 px faint word); `crates/ui/src/transcript/agent_cards.rs:335-360` (6 px dot + word); Agents pane (`native_agents`). Evidence: `shots/timeline/child-transcript-tool-detail.png` vs `shots/timeline/agents-pane-status-words.png`.
- **What's wrong:** The same three agents read "Done / Interrupted / Failed" on transcript chips but "Completed / Stopped / Failed" in the Agents pane. The chip shows a bare word, the creation card a dot plus word, and the pane a dot plus word plus prose.
- **Why it hurts the premium feel:** Users can't tell whether "Interrupted" and "Stopped" are different states, and the visual grammar changes per surface.
- **Proposed fix:** One lexicon (Running, Waiting, Idle, Done, Stopped, Failed, Not observed) and one `StatusBadge(status)`: a 6 px dot (the item spinner while running) plus an 11 px medium word, with red only for Failed. Use it in chips, creation cards and the Agents pane.
- **Before:** `mocks/TL/TL-22-before.svg`, `shots/timeline/agents-pane-status-words.png`
- **After:** `mocks/TL/TL-22-after.svg`
- **Effort:** S

### TL-23 — Light theme: the user bubble barely has an edge and a pending send reads as disabled
- **Severity:** P3
- **Category:** color/contrast · states
- **Where:** `crates/ui/src/theme.rs:2308-2313` (`user_bubble_bg` light = `wash(0.04)`), `crates/ui/src/transcript.rs:7026` (`pending` → whole bubble `opacity(0.65)`). Evidence: `shots/timeline/light-bubbles-pending.png` (Luna Light).
- **What's wrong:** In Luna Light the bubble paints #F2F3F5 on the #FBFCFE canvas, 1.08:1, so its edge nearly disappears. A pending send dims the entire bubble to 65%, so its text drops to ≈#6B6F77 and looks disabled rather than in flight.
- **Why it hurts the premium feel:** The user's own words should be the most legible thing on screen, and a send in progress should look in progress, not greyed out.
- **Proposed fix:** Use a light plate of `wash(0.065)` (≈#ECEDF0) with a 0.75 px `hairline(0.06)` edge. Pending keeps full-contrast text and adds a 12 px clock icon and the caption "Sending…" (11 px tertiary) under the bubble, which disappears when the echo lands.
- **Before:** `mocks/TL/TL-23-before.svg`, `shots/timeline/light-bubbles-pending.png`
- **After:** `mocks/TL/TL-23-after.svg`
- **Effort:** S

---

## Top 5 fixes with the biggest premium payoff

1. **TL-24 — Working indicator → live status (option C with A's motion).** It is on screen for every second of every turn. A breathing orb, a shimmering verb that names the actual work and a quiet tabular timer change how the whole app feels.
2. **TL-01 + TL-02 — One spring disclosure with a rotating, always-visible chevron.** Expanding tool calls is the core timeline gesture. A 300 ms spring with a content fade replaces the 140 ms clip-wipe and the hidden, glyph-swapping chevrons.
3. **TL-05 + TL-04 — Restore the tone ladder and stop painting failures red.** Three real text tiers plus "red glyph + red word" for failures calm every dense tool group.
4. **TL-06 + TL-11 + TL-12 — One `code_surface` for output, invocation, diff and fences.** It removes the r7/r10 mismatch, double hairlines, ellipsised error paths and the frameless diff in one extraction.
5. **TL-03 — Reflow thought text to the real width.** The reasoning block becomes readable in side panes and uses the full column on wide screens, with an analytic height preserved.
