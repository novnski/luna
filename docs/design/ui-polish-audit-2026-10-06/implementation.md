# Implementation tracker

Status of every audit finding. `todo` · `wip` · `done` (commit) · `wontfix` (reason).
Update this file as each finding lands so work can resume after compaction.

| ID | Sev | Title | Status | Notes |
| --- | --- | --- | --- | --- |
| TL-24 | P1 | Working indicator: sparse flickering dots, random whimsical verb, flat jumping timer (user-requested) | done (milestone A) | Breathing orb, live status from the newest unresolved tool, shimmer, tabular timer, hover Stop; flavour rotation retired |
| TL-01 | P1 | Expanding a tool row is a 140 ms height-only wipe; four different fold clocks | done (milestone A) | motion::DISCLOSURE (300 ms) for tool rows/groups/diff collapse; body fades in 180 ms, out by 60% |
| TL-02 | P1 | Disclosure chevrons: three sizes, two behaviours, hidden until hover | done (milestone A) | One 12 px alt-arrow-right rotated by fold progress; always visible at 50%; right-aligned accessory column |
| TL-03 | P1 | Thought text is hard-wrapped at 96 columns, then each line is truncated | done (milestone G) | Thought details wrap to the live column (ThoughtColsScope + width-keyed cache); analytic heights use the same budget |
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
| CP-01 | P1 | Model and access menus open detached from their chips | done (milestone C) | popover::anchored_menu_above_leading / below_leading (−CARD_INSET) for model and access menus |
| CP-02 | P1 | Hover tooltips cover the control they describe | done (milestone C) | popover::TooltipAboveExt — tooltips centered above the trigger, 400 ms delay; composer chips, attach, goal, usage, send |
| CP-03 | P1 | Placeholder is almost as bright as typed text (flat tonal hierarchy) | done (milestone C) | Theme::placeholder floored on the composer plate for the message composer |
| CP-04 | P1 | Integration rows: the subtitle lands as a zig-zag trailing column | done (milestone C) | Integration rows: glyph · title-over-subtitle stack · right-aligned kind |
| CP-05 | P1 | Completion popup slices the new-thread destination row | done (milestone C) | Destination row hidden while a completion popup is open |
| CP-06 | P1 | Controls row overflows at ~300pt panes (the parity fixture fails) | done (milestone C) | 28 pt controls free the 2 pt; parity fixture passes 300 pt (fixture uses ACCESS_CHIP_SIZE) |
| CP-07 | P1 | One controls row, six control styles | done (milestone C) | pickers::CONTROL_HEIGHT 28 capsule for model/access/goal/usage/work chips |
| CP-08 | P1 | Popover entrance: wrong direction, opacity pop, blur snaps in | done (milestone C) | menu_in_from: full fade + 6 pt travel from the trigger side, 180 ms ease-out-quint; exit 120 ms opacity only |
| CP-09 | P1 | Send looks ready on an empty draft; blocked and queue states are silent | done (milestone C) | One disc: recessed when empty/blocked with reason tooltip; queue glyph; Send/Queue tooltips |
| CP-10 | P2 | Send ↔ Stop is an instant element swap; hover is an instant opacity step | done (milestone C) | Stable composer-primary disc, glyph crossfade, hover_blend, pressed state |
| CP-11 | P2 | Completion popups vanish without an exit; the stash list is a hand-rolled card | done (milestone H) | `@`, `#`, `/` and stash popups hold their last open state for `MENU_OUT` after closing (`CompletionExits`), rendered with the menu-out fade and dead hit-testing; the stash list is rebuilt on `full_width_menu_above` + the menu card, `menu_row` and a kit Delete button |
| CP-12 | P2 | Five tooltip implementations in one composer | done (milestone C) | Shared tooltip_card/TextTooltip; ControlTooltip, appshot and queue tooltips routed through it |
| CP-13 | P2 | Two highlighted rows at once; the default cursor lands on a disabled row | done (milestone H) | `menu_row_nav` paints one cursor wash shared by arrows and pointer (`.on_mouse_move`, never on scroll) with the current choice as a trailing check; `menu_row_cursor` for the completions; Access opens on its current choice or the first available one and arrows skip unavailable rows (`menu_step_where`) |
| CP-14 | P2 | Seven type sizes and two heading styles across sibling menus | done (milestone C) | menu_heading 11 pt semibold (no sub-11 menu text) |
| CP-15 | P2 | The composer pill has no focused state | done (milestone C) | Composer pill: border_strong + inset accent ring when focused |
| CP-16 | P2 | Floating menus cast no shadow while tooltips do (elevation inverted) | done (milestone H) | Frosted cards get `Frosted::elevated()`: a contact rim and soft downward rings painted outside the card after its blur (no box shadow, so the glass interior never stains); menus, nested menus, dialogs, the palette, hover cards and the outline popover opt in; opaque cards keep `menu_shadows`/`tooltip_shadows` |
| CP-17 | P2 | Unknown context usage renders as "○ —" | done (milestone C) | Unknown usage shows the bare ring; tabular share label |
| CP-18 | P2 | The text column and the control column start at different x | done (milestone C) | ACTIONS_ROW_INSET 12 → 10 |
| CP-19 | P2 | Attachment remove control: 18pt, hover-only, double ring; thumbnail radii drift | done (milestone C) | 20 pt raised remove disc with plain ×, visible on hover and focus, Role::Button |
| CP-20 | P2 | Stacked cards above the composer use four unrelated radii | done (milestone H) | Stack ladder in `popover`: full-width popups r20 (inset 6, rows r14, matching frost mask), queue and goal trays r18, composer notices and the unsent-recovery card r14, under the r26 pill |
| CP-21 | P2 | Queue row: tiny drag handle, r5 buttons, instant hovers, mixed action styles | done (milestone C) | Queue handle 20×28 hit area; actions as 28 pt circles with focus ring; Steer 12/500 |
| CP-22 | P3 | Goal bar: orphan separator dot and a jittering timer | done (milestone C) | Separator only between title and objective; tabular timer |
| CP-23 | P3 | The draft notice repeats its title and spends a row on a text link | done (milestone C) | Notice action in the title row as a 24 pt capsule; duplicated title removed from the message |
| CP-24 | P3 | Five popover widths and three hairline alphas | done (milestone C) | MENU_HAIRLINE 0.08; MENU_W_S/M/L tokens for picker widths |
| SH-01 | P0 | Composer notice is a 5% tint, so transcript text shows straight through it | done (milestone A) | Opaque plate (warning 8% flattened over the canvas) + 22% ring |
| SH-02 | P1 | The selected sidebar row paints exactly the hover fill | done (milestone D) | glass_selected_bg 0.16/0.10 vs sidebar_hover_bg 0.06/0.04 (no ring, per earlier user request) |
| SH-03 | P1 | The tab strip hard-clips tabs, and Main can scroll out of view | done (milestone D) | Tab scroller wrapped in edge_faded(24).fade_overflow_x |
| SH-04 | P1 | The command palette re-centres while you type, and appears in a single frame | done (milestone D) | Palette top-anchored at max(88, 20% vh); 160 ms fade + 6 pt rise |
| SH-05 | P1 | The titlebar repeats the tab's title, on a different left edge | done (milestone D) | Titlebar carries context only: project (13/600) › device |
| SH-06 | P1 | Two tab systems in one window, and their header bands sit 6 px apart | done (milestone D) | Right-pane tabs 28 px / r8 / 12.5 px like canvas tabs; canvas tabs r8 |
| SH-07 | P1 | Light mode: the content plane is a grey tray, darker than the chrome | done (milestone D) | Light canvas_fill = bg @ 0.96; frost density still checked against the old recessed plane (contrast audit passes) |
| SH-08 | P1 | Light mode: disabled Back/Forward look exactly like enabled controls | done (milestone D) | Theme::disabled_glyph (muted @ 35%) for disabled back/forward |
| SH-09 | P1 | The sidebar "View options" button keeps a focus ring after mouse use | done (milestone D) | View options uses focus_visible ring instead of in_focus border |
| SH-10 | P2 | Chrome controls and icons have no size ladder | done (milestone H) | `surface_chrome` ladder CONTROL_S/M/L 20/28/32, ICON_S/M/L 12/16/18, radius 4/6/8: titlebar, Linux captions, header icons, right-pane add, strip buttons and sidebar header controls at M; row actions and tab closes at S; surface toolbars and fields move to M |
| SH-11 | P2 | The three sidebar footer buttons disagree on size, tone and hover | done (milestone G) | Footer buttons one recipe: 32 pt, 18 pt icon, muted tone, shared hover; open usage card shows selected wash |
| SH-12 | P2 | Hover feedback mixes timing and uses seven different washes | done (milestone H) | Every shell hover (sidebar rows, row actions, footer, tabs, titlebar, palette tiles, strip and pane buttons) eases through `interaction::HoverFill` on the shared fade, with the `FillTier` weights |
| SH-13 | P2 | Sidebar rows lead with the project, and the trailing corner keeps changing meaning | done (milestone H) | Rows are a title line (13/500 with the harness mark, time at right; hover actions replace only the time) over a meta line (status slot, status word, project unless one project is filtered, branch or note, PR badge); 52 pt rows, drag ghost matches; user-requested row-hover swap and no-ring selection kept |
| SH-14 | P2 | Active, settled and archived rows use different geometry in one column | done (milestone H) | One leading grid: a 16 pt mark slot + 8 pt gap for active, settled and archived rows (titles at x = 40; settled rows gain the harness mark); marks keep the 13/14 pt hierarchy inside the slot; geometry test updated to the shared column |
| SH-15 | P2 | Sidebar section headers: hairline rules, always-on chevrons, uneven grouping | done (milestone G) | Section headers 11 pt semibold faint, no rule, chevron on hover, hover wash |
| SH-16 | P2 | Sidebar collapse is a 200 ms clip-wipe | done (milestone D) | Sidebar content slides with the edge and fades over the last 40% of travel |
| SH-17 | P2 | Switching chats blanks the transcript, then pops the new one in | done (milestone G) | Selected transcript fades and settles in (160 ms) once its geometry is ready |
| SH-18 | P2 | Two different splitters in one window | done (milestone G) | Workbench divider 10 pt hit, neutral hairline on hover, accent only while dragging |
| SH-19 | P2 | Four keyboard-focus ring recipes, two of them nearly invisible | done (milestone D) | Every shell border-based focus style replaced with theme::focus_ring |
| SH-20 | P2 | Chrome text drops to 9–10 px, across 15 sizes in the shell | done (milestone D) | All 9–10.5 px shell/palette/popover text raised to 11 px |
| SH-21 | P2 | Two tooltip styles and three delays | done (milestone D) | SurfaceTabTooltip renders the shared tooltip card; right tabs use text_tooltip |
| SH-22 | P2 | Tab activation reflows the strip, and the close glyph snaps | done (milestone D) | One tab weight (Medium); activation via fill and colour |
| SH-23 | P2 | The right-pane picker looks like a wireframe, on a different surface from the centre | done (milestone G) | Surface picker: filled tiles, no outlines, heading and hint |
| SH-24 | P2 | Three empty states, three different visual languages | done (milestone G) | Sidebar empty state centred with title and next step using the user's actual shortcut (canvas stays untitled per earlier user request) |
| SH-25 | P3 | Palette rows: three heights, tiny headers, non-concentric radius | done (milestone D) | PALETTE_ITEM_RADIUS 16−8; palette rows 32 / 44 |
| SH-26 | P3 | The Activity bell's unread dot floats off the glyph | done (milestone D) | 6 pt unread dot seated on the bell shoulder |
| DS-01 | P1 | No type scale; 17 sizes and borrowed 1.618 leading | done (milestone F) | typography::TextStyle (caption2…display) + TypeStyleExt::type_style; used for new UI, page titles |
| DS-02 | P1 | Nine button/picker recipes; five heights on one page | done (milestone H) | `button.rs` kit: `button`/`icon_button`/`select`/`select_frame`/`chip` in Sm 24 / Md 28 / Lg 32 with Primary/Secondary/Ghost/Danger; Settings ghost actions, dialog buttons, theme and font pickers (one chevron), MCP and git buttons rebuilt on it |
| DS-03 | P1 | Settings row text runs under its control; no reflow | done (milestone F) | meta_line fragments shrink and wrap instead of running under controls |
| DS-04 | P1 | macOS "Reduce motion" is never read | done (milestone A) | appearance::observe_accessibility reads NSWorkspace reduce motion on launch and activation |
| DS-05 | P1 | 25 hover fills and two hover behaviours | done (milestone H) | `interaction::HoverFill` (hover_fill/_fg/_wash/_border/_opacity/_colors) on `motion::HOVER_FADE`; all ~170 `.hover()` fills migrated except the composer files (milestone I) and scrollbar thumbs; Settings card rows no longer hover (not controls) |
| DS-06 | P1 | Switches, segments and swatches snap; the switch is off-brand | done (milestone H) | Switch knob travels and track crossfades on `motion::DISCLOSURE` only when the value flips (`motion::follow`, keyed per switch; a new key settles, so mounts never replay); segmented thumbs slide on `motion::QUICK` |
| DS-07 | P1 | Settings group headers float between groups | done (milestone F) | Group headers 24 pt above / 8 pt to their card (field_label margins; label-led cards drop mt) |
| DS-08 | P1 | Three settings-row anatomies, all oversized | done (milestone F) | card_row 16/10 padding, 12 gap; row tile 28 r7 (~52 pt rows) |
| DS-09 | P1 | Icon set is not uniformly Solar Linear | done (milestone F) | 37 glyphs replaced with official Solar Linear (pixel-compared); stop, window controls, return and queue family stay custom |
| DS-10 | P1 | 17 corner radii, colliding names, non-concentric nesting | done (milestone F) | theme::radius ladder (XS/SM/MD/LG/XL + inner); timeline migrated |
| DS-11 | P2 | Spacing is half off the 4-pt grid | done (milestone F) | theme::space ladder on the 4 pt grid |
| DS-12 | P2 | 17 icon sizes; no icon tokens or icon+label helper | done (milestone F) | icons::size ladder (XS 12 / SM 14 / MD 16 / LG 20) |
| DS-13 | P2 | Secondary text has no defined tiers (two greys on one page) | done (milestone F) | meta_line uses readable_muted (secondary tier floored); content tones for the canvas |
| DS-14 | P2 | Fills and hairlines are 27 raw alphas, untinted by the theme | done (milestone H) | `Theme::fill(FillTier)` (4/8/12/16% dark, 3/5/8/10% light) and `Theme::separator(strong)` tinted by the palette's text; free `fill()`/`separator()` for context-free builders; `glass_hover`, selection and sidebar hover use them |
| DS-15 | P2 | Motion catalog fragmented: 16 durations, 6 curves, no spring | done (milestone F) | Motion tokens MICRO/QUICK/STANDARD/EMPHASIZED/DISCLOSURE + curves; menus, disclosures, palette, toasts use them |
| DS-16 | P2 | Five ways to show "selected"; no shared segmented control | done (milestone H) | `button::segmented` (sliding thumb) for Glass, background effect, diff layout/colors, send shortcut, follow-up, provider and launch-mode choices; `button::selection_ring` for accent swatches and option cards; `chip` for many-option choices |
| DS-17 | P2 | Seven focus treatments; most clickables are mouse-only | done (milestone F) | One focus ring recipe across transcript, shell, composer, surfaces; theme::focus_visible_ring helper |
| DS-18 | P2 | Sub-11 pt text in everyday chrome | done (milestone E) | No ui_rems text below 11 pt remains in the UI crate |
| DS-19 | P2 | 16–18 pt click targets | done (milestone F) | 24 pt hit boxes with negative margins for the 16–18 pt targets |
| DS-20 | P3 | Six tooltip delays, no warm-up, no fade | done (milestone F) | popover::TOOLTIP_DELAY 400 ms everywhere (250/300/350 removed); shared TextTooltip |
| DS-21 | P3 | One glyph, many meanings in Settings | done (milestone F) | Settings rows use palette / magic wand / columns / restart; General vs Appearance rail glyphs distinct |
| DS-22 | P3 | Page, group and row titles look alike | done (milestone F) | Page title title1 20/25 semibold; group headers 13 semibold secondary; rows 13 medium |
| SS-01 | P1 | Right pane content swaps to the surface picker (or blanks) while the pane closes | done (milestone E) | Closing pane renders empty instead of flashing the surface picker |
| SS-02 | P1 | Diff rows: no word-level highlight, near-invisible tint, four redundant add/remove signals | done (milestone E) | Row wash 0.10/0.08, word-level highlights (similar word diff, 0.28/0.20), no edge bar, marker 16, quiet line numbers |
| SS-03 | P1 | The diff view and 12 other scroll regions have no scrollbar at all | done (milestone E) | Diff list gets the shared floating rail (popover::list_rail_geometry); other lists remain |
| SS-04 | P1 | Usage meters: Codex is always red, the card shows "used" while the page shows "left", 0% draws a stub | done (milestone E) | usage_meter: fill = used, neutral/warning 75/danger 90, 6 pt capsule, no stub; card and page |
| SS-05 | P1 | Terminal trackpad scrolling drops small deltas and jumps in whole lines | done (milestone E) | Terminal accumulates sub-line scroll remainder |
| SS-06 | P1 | The right pane's toolbar row sits about 6.5 pt above the canvas tab strip | done (milestone E) | Right pane top padding = titlebar + 6 (canvas strip line) |
| SS-07 | P1 | Three different tab recipes across canvas, right pane and terminal drawer | done (milestone E) | Terminal tab bar uses HEADER_HEIGHT 38 (tab geometry already 28/r8) |
| SS-08 | P1 | No shared toast: feedback snaps into the left sidebar, one slot, four notice recipes | done (milestone E) | Canvas toast capsule above the composer (fade+rise, dismiss); sidebar copy only when no composer |
| SS-09 | P1 | The image lightbox snaps open and closed with no zoom origin, controls or readout | done (milestone E) | Lightbox backdrop fade, image rise, close capsule, readable caption |
| SS-10 | P2 | File tree selection is a full-bleed band; focus and blur look the same; no indent guides | done (milestone E) | Inset rounded tree selection; accent when focused; rotating chevron |
| SS-11 | P2 | Disclosure chevrons behave three different ways | done (milestone E) | Diff header + tree chevrons rotate one ALT_ARROW_RIGHT (disclosure curve); stale CHEVRON comment fixed |
| SS-12 | P2 | Empty, loading and error states: six recipes, one spinner, one blank pane | done (milestone E) | surface_chrome::surface_state + state_glyph; Files loading/error; preview states on the shared body |
| SS-13 | P2 | Git card: four identical buttons, no primary action, a toggle with no state | done (milestone E) | git_primary_button (accent) for Commit; 26 pt buttons, 12 pt labels, Role::Button |
| SS-14 | P2 | Pane content does not share the toolbar's left edge (8 vs 12 vs 16 vs 20 pt) | done (milestone E) | surface_chrome::CONTENT_INSET 12 for hunk headers, agents empty state, previews |
| SS-15 | P2 | Agents surface: a wall of engine diagnostics with two row recipes in one pane | done (milestone G) | Agents and Work rows: status dot + title, one quiet meta line, engine detail in tooltip |
| SS-16 | P2 | Keyboard focus is invisible on most secondary-surface controls | done (milestone E) | focus rings on files toolbar, diff header toggles, git buttons (theme::focus_visible_ring helper) |
| SS-17 | P2 | Type and radius sprawl: 15 text sizes, 9 radii, px line-heights under rem text | done (milestone E) | All ui_rems text below 11 pt raised to 11 across the crate |
| SS-18 | P2 | Files status text below the readable floor (10–10.5 px, 10.5 px icon) | done (milestone E) | Files status text raised to 11+ |
| SS-19 | P2 | Editor breadcrumb truncates every segment and buries the file name | done (milestone E) | Breadcrumb leaf 12 pt medium primary, never shrinks first; chevron separators |
| SS-20 | P3 | Diff hunk headers sit on their own grid and scale with the wrong setting | done (milestone E) | Hunk header sized from code font, content inset |
| SS-21 | P3 | Browser toolbar: no load progress, Reload never becomes Stop, the error border shifts the field | done (milestone E) | Address field reserves its border; accent loading line; Go button r3 (Stop not implemented: no stop API) |
