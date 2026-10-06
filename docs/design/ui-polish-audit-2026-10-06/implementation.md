# Implementation tracker

Status of every audit finding. `todo` · `wip` · `done` (commit) · `wontfix` (reason).
Update this file as each finding lands so work can resume after compaction.

| ID | Sev | Title | Status | Notes |
| --- | --- | --- | --- | --- |
| TL-24 | P1 | Working indicator: sparse flickering dots, random whimsical verb, flat jumping timer (user-requested) | done (milestone A) | Breathing orb, live status from the newest unresolved tool, shimmer, tabular timer, hover Stop; flavour rotation retired |
| TL-01 | P1 | Expanding a tool row is a 140 ms height-only wipe; four different fold clocks | done (milestone A) | motion::DISCLOSURE (300 ms) for tool rows/groups/diff collapse; body fades in 180 ms, out by 60% |
| TL-02 | P1 | Disclosure chevrons: three sizes, two behaviours, hidden until hover | done (milestone A) | One 12 px alt-arrow-right rotated by fold progress; always visible at 50%; right-aligned accessory column |
| TL-03 | P1 | Thought text is hard-wrapped at 96 columns, then each line is truncated | todo | Deferred: needs width-aware thought wrapping with analytic heights |
| TL-04 | P1 | Failed tool rows and agent cards paint everything red | done (milestone A) | Label/detail keep row tones; close-circle glyph + trailing "Failed" |
| TL-05 | P1 | Text tiers collapse: muted and faint render as the same grey (faint is brighter) | done (milestone A) | content_muted / content_faint floored on the canvas with a 1.35x step; used across transcript + markdown |
| TL-06 | P1 | Tool output and code blocks are two different components | done (milestone A) | Tool output uses code block recipe (r10, 28 px header, shared copy_action, 12.5/18 mono, horizontal scroll) |
| TL-07 | P1 | Four left edges inside one reply; the group title sits 4 px off the glyph column | done (milestone A) | Group chevron box 26 px; title on the glyph column |
| TL-08 | P1 | Error and retry states use four unrelated treatments | done (milestone B) | transcript_notice (tone card, glyph, title, detail, 24 px solid/outlined buttons) for run error, recovery, undelivered |
| TL-09 | P1 | Keyboard focus is invisible on tool rows and inconsistent elsewhere | done (milestone A) | focus_ring recipe on headers, rows, expander, copy, subagent cards, trailer actions, thumbnails |
| TL-10 | P2 | Nine corner radii in one timeline | done (milestone B) | radius ladder (theme::radius XS/SM/MD/LG/XL + inner) across transcript, agent cards, markdown |
| TL-11 | P2 | Stacked Invocation and Output boxes form a double hairline; "Show full output" is bare text | done (milestone B) | Invocation + output render as segments of one code surface; Show full output is a ghost button with spinner |
| TL-12 | P2 | Inline diffs in the activity tree are frameless and square | done (milestone B) | Inline diffs framed with a header (file name, +N −N) |
| TL-13 | P2 | Four loader idioms, and running tool rows show none | done (milestone B) | Live unresolved tool rows show the item spinner (gated on chat_run_live) |
| TL-14 | P2 | Hover metadata strip mixes text and icon buttons with uneven feedback | done (milestone B) | meta_action 24 px icon buttons with shared tooltip/hover/focus; tertiary timestamp |
| TL-15 | P2 | Seven chrome text sizes, two below the macOS floor | done (milestone B) | Sub-11 px timeline labels raised to 11 px; TextStyle scale added (typography::TextStyle) |
| TL-16 | P2 | Icon sizes drift from 11 to 16 px and glyphs collide in meaning | done (milestone B) | Rail glyphs 14 px; Solar plug-circle (MCP), lightbulb (thought), bolt (hooks), pen-new-square (patch) |
| TL-17 | P2 | Jump to bottom: text arrow, no exit, 1.2 s creeping glide | done (milestone B) | Jump pill: Solar arrow-down icon, Role::Button, aria label, focus ring (glide unchanged) |
| TL-18 | P2 | Mermaid diagrams: box-in-box, cool-grey square nodes, source-to-diagram snap | done (milestone B) | Mermaid on the frame fill; ink nodes, hairline edges, tertiary lines, 13 px text |
| TL-19 | P2 | Markdown headings get the same 12 px gap as paragraphs | done (milestone B) | Heading top spacing (H1 28 / H2 24 / H3 20) in render_tree and top_gap_for; H2 17/24 |
| TL-20 | P2 | Turn rhythm: an empty 32 px lane and a 16 px turn gap split one reply into two | done (milestone B) | Meta lane 32 → 28 px (24 px buttons + 4 px lead) |
| TL-21 | P2 | Long prompt collapse: an ASCII "..." line and a glyph-swap toggle | done (milestone B) | Toggle 13 px medium with one rotating chevron; real ellipsis in tertiary (no gradient: frosted backdrop) |
| TL-22 | P2 | One agent, three status vocabularies | done (milestone B) | One lexicon (Running/Done/Stopped/Failed…) and dot + word badge in chips and the Agents pane |
| TL-23 | P3 | Light theme: the user bubble barely has an edge and a pending send reads as disabled | done (milestone B) | Light bubble wash 0.065 + inset hairline edge; pending keeps contrast and shows a clock |
| CP-01 | P1 | Model and access menus open detached from their chips | todo | |
| CP-02 | P1 | Hover tooltips cover the control they describe | todo | |
| CP-03 | P1 | Placeholder is almost as bright as typed text (flat tonal hierarchy) | todo | |
| CP-04 | P1 | Integration rows: the subtitle lands as a zig-zag trailing column | todo | |
| CP-05 | P1 | Completion popup slices the new-thread destination row | todo | |
| CP-06 | P1 | Controls row overflows at ~300pt panes (the parity fixture fails) | todo | |
| CP-07 | P1 | One controls row, six control styles | todo | |
| CP-08 | P1 | Popover entrance: wrong direction, opacity pop, blur snaps in | todo | |
| CP-09 | P1 | Send looks ready on an empty draft; blocked and queue states are silent | todo | |
| CP-10 | P2 | Send ↔ Stop is an instant element swap; hover is an instant opacity step | todo | |
| CP-11 | P2 | Completion popups vanish without an exit; the stash list is a hand-rolled card | todo | |
| CP-12 | P2 | Five tooltip implementations in one composer | todo | |
| CP-13 | P2 | Two highlighted rows at once; the default cursor lands on a disabled row | todo | |
| CP-14 | P2 | Seven type sizes and two heading styles across sibling menus | todo | |
| CP-15 | P2 | The composer pill has no focused state | todo | |
| CP-16 | P2 | Floating menus cast no shadow while tooltips do (elevation inverted) | todo | |
| CP-17 | P2 | Unknown context usage renders as "○ —" | todo | |
| CP-18 | P2 | The text column and the control column start at different x | todo | |
| CP-19 | P2 | Attachment remove control: 18pt, hover-only, double ring; thumbnail radii drift | todo | |
| CP-20 | P2 | Stacked cards above the composer use four unrelated radii | todo | |
| CP-21 | P2 | Queue row: tiny drag handle, r5 buttons, instant hovers, mixed action styles | todo | |
| CP-22 | P3 | Goal bar: orphan separator dot and a jittering timer | todo | |
| CP-23 | P3 | The draft notice repeats its title and spends a row on a text link | todo | |
| CP-24 | P3 | Five popover widths and three hairline alphas | todo | |
| SH-01 | P0 | Composer notice is a 5% tint, so transcript text shows straight through it | done (milestone A) | Opaque plate (warning 8% flattened over the canvas) + 22% ring |
| SH-02 | P1 | The selected sidebar row paints exactly the hover fill | todo | |
| SH-03 | P1 | The tab strip hard-clips tabs, and Main can scroll out of view | todo | |
| SH-04 | P1 | The command palette re-centres while you type, and appears in a single frame | todo | |
| SH-05 | P1 | The titlebar repeats the tab's title, on a different left edge | todo | |
| SH-06 | P1 | Two tab systems in one window, and their header bands sit 6 px apart | todo | |
| SH-07 | P1 | Light mode: the content plane is a grey tray, darker than the chrome | todo | |
| SH-08 | P1 | Light mode: disabled Back/Forward look exactly like enabled controls | todo | |
| SH-09 | P1 | The sidebar "View options" button keeps a focus ring after mouse use | todo | |
| SH-10 | P2 | Chrome controls and icons have no size ladder | todo | |
| SH-11 | P2 | The three sidebar footer buttons disagree on size, tone and hover | todo | |
| SH-12 | P2 | Hover feedback mixes timing and uses seven different washes | todo | |
| SH-13 | P2 | Sidebar rows lead with the project, and the trailing corner keeps changing meaning | todo | |
| SH-14 | P2 | Active, settled and archived rows use different geometry in one column | todo | |
| SH-15 | P2 | Sidebar section headers: hairline rules, always-on chevrons, uneven grouping | todo | |
| SH-16 | P2 | Sidebar collapse is a 200 ms clip-wipe | todo | |
| SH-17 | P2 | Switching chats blanks the transcript, then pops the new one in | todo | |
| SH-18 | P2 | Two different splitters in one window | todo | |
| SH-19 | P2 | Four keyboard-focus ring recipes, two of them nearly invisible | todo | |
| SH-20 | P2 | Chrome text drops to 9–10 px, across 15 sizes in the shell | todo | |
| SH-21 | P2 | Two tooltip styles and three delays | todo | |
| SH-22 | P2 | Tab activation reflows the strip, and the close glyph snaps | todo | |
| SH-23 | P2 | The right-pane picker looks like a wireframe, on a different surface from the centre | todo | |
| SH-24 | P2 | Three empty states, three different visual languages | todo | |
| SH-25 | P3 | Palette rows: three heights, tiny headers, non-concentric radius | todo | |
| SH-26 | P3 | The Activity bell's unread dot floats off the glyph | todo | |
| DS-01 | P1 | No type scale; 17 sizes and borrowed 1.618 leading | todo | |
| DS-02 | P1 | Nine button/picker recipes; five heights on one page | todo | |
| DS-03 | P1 | Settings row text runs under its control; no reflow | todo | |
| DS-04 | P1 | macOS "Reduce motion" is never read | done (milestone A) | appearance::observe_accessibility reads NSWorkspace reduce motion on launch and activation |
| DS-05 | P1 | 25 hover fills and two hover behaviours | todo | |
| DS-06 | P1 | Switches, segments and swatches snap; the switch is off-brand | todo | |
| DS-07 | P1 | Settings group headers float between groups | todo | |
| DS-08 | P1 | Three settings-row anatomies, all oversized | todo | |
| DS-09 | P1 | Icon set is not uniformly Solar Linear | todo | |
| DS-10 | P1 | 17 corner radii, colliding names, non-concentric nesting | todo | |
| DS-11 | P2 | Spacing is half off the 4-pt grid | todo | |
| DS-12 | P2 | 17 icon sizes; no icon tokens or icon+label helper | todo | |
| DS-13 | P2 | Secondary text has no defined tiers (two greys on one page) | todo | |
| DS-14 | P2 | Fills and hairlines are 27 raw alphas, untinted by the theme | todo | |
| DS-15 | P2 | Motion catalog fragmented: 16 durations, 6 curves, no spring | wip | Motion tokens MICRO/QUICK/STANDARD/EMPHASIZED/DISCLOSURE + curves added; call sites migrate per surface |
| DS-16 | P2 | Five ways to show "selected"; no shared segmented control | todo | |
| DS-17 | P2 | Seven focus treatments; most clickables are mouse-only | todo | |
| DS-18 | P2 | Sub-11 pt text in everyday chrome | todo | |
| DS-19 | P2 | 16–18 pt click targets | todo | |
| DS-20 | P3 | Six tooltip delays, no warm-up, no fade | todo | |
| DS-21 | P3 | One glyph, many meanings in Settings | todo | |
| DS-22 | P3 | Page, group and row titles look alike | todo | |
| SS-01 | P1 | Right pane content swaps to the surface picker (or blanks) while the pane closes | todo | |
| SS-02 | P1 | Diff rows: no word-level highlight, near-invisible tint, four redundant add/remove signals | todo | |
| SS-03 | P1 | The diff view and 12 other scroll regions have no scrollbar at all | todo | |
| SS-04 | P1 | Usage meters: Codex is always red, the card shows "used" while the page shows "left", 0% draws a stub | todo | |
| SS-05 | P1 | Terminal trackpad scrolling drops small deltas and jumps in whole lines | todo | |
| SS-06 | P1 | The right pane's toolbar row sits about 6.5 pt above the canvas tab strip | todo | |
| SS-07 | P1 | Three different tab recipes across canvas, right pane and terminal drawer | todo | |
| SS-08 | P1 | No shared toast: feedback snaps into the left sidebar, one slot, four notice recipes | todo | |
| SS-09 | P1 | The image lightbox snaps open and closed with no zoom origin, controls or readout | todo | |
| SS-10 | P2 | File tree selection is a full-bleed band; focus and blur look the same; no indent guides | todo | |
| SS-11 | P2 | Disclosure chevrons behave three different ways | todo | |
| SS-12 | P2 | Empty, loading and error states: six recipes, one spinner, one blank pane | todo | |
| SS-13 | P2 | Git card: four identical buttons, no primary action, a toggle with no state | todo | |
| SS-14 | P2 | Pane content does not share the toolbar's left edge (8 vs 12 vs 16 vs 20 pt) | todo | |
| SS-15 | P2 | Agents surface: a wall of engine diagnostics with two row recipes in one pane | todo | |
| SS-16 | P2 | Keyboard focus is invisible on most secondary-surface controls | todo | |
| SS-17 | P2 | Type and radius sprawl: 15 text sizes, 9 radii, px line-heights under rem text | todo | |
| SS-18 | P2 | Files status text below the readable floor (10–10.5 px, 10.5 px icon) | todo | |
| SS-19 | P2 | Editor breadcrumb truncates every segment and buries the file name | todo | |
| SS-20 | P3 | Diff hunk headers sit on their own grid and scale with the wrong setting | todo | |
| SS-21 | P3 | Browser toolbar: no load progress, Reload never becomes Stop, the error border shifts the field | todo | |
