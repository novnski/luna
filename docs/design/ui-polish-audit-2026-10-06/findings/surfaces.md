# Secondary surfaces (SS)

Source baseline `590cf4b6`. Mocks: `mocks/SS/`. Real renders: `shots/surfaces/`.

## Area overview

The secondary surfaces (Files, Diff/Git/PR, Terminal, Browser, image viewer,
notices, Usage, Agents/Automations) share a good foundation: `surface_chrome.rs`
gives Files, Diff, History and Browser one 38 px toolbar with 24 px / r6 icon
buttons and one 24 px input. Most surfaces still miss that shared recipe in
some way. The pane width animates over 200 ms, but its content snaps or swaps
while closing. Each surface has its own tab, notice, empty state, loader and
button. Only the Files tree and the Terminal use the shared overlay
scrollbar. Diffs carry four redundant add/remove signals but no word-level
highlight. The usage meters disagree on colour and meaning between the card
and the page. Real renders were taken from the `usage-fixture`,
`chat-workbench-fixture` and `native-agents-fixture` runs. Files, Diff,
Terminal and Browser have no headless fixture, so their findings come from
measured code values; `browser-fixture` and `preview-fixture` are not in the
shared build because they need other features. All measurements are in
logical points at the default 16 px UI size.

---

### SS-01 — Right pane content swaps to the surface picker (or blanks) while the pane closes
- **Severity:** P1
- **Category:** motion
- **Where:** `crates/ui/src/shell.rs:13051-13063`, `crates/ui/src/shell.rs:13117-13119`, `crates/ui/src/shell.rs:13156`; tween `crates/ui/src/motion.rs:383` (`RESIZE` 200 ms ease-out)
- **What's wrong:** The right pane keeps rendering for the whole 200 ms width tween, but the match arms for `Agents`, `Automations`, `RelatedChats` and `Subagent` are guarded by `if self.right_pane_open(cx)`. Once the toggle flips, those arms stop matching and fall through to `_ => self.render_surface_picker(cx)`. So for about 12 frames the closing pane shows the "Files / Terminal / Diffs / History" picker instead of the surface you were looking at. `Files`/`File` render `gpui::Empty` (line 13062), so the Files pane slides out as a blank frosted slab. Content has no opacity fade, only the width moves.
- **Why it hurts the premium feel:** A closing panel that changes what it shows reads as a glitch. macOS sidebars carry their content out unchanged and fade it.
- **Proposed fix:** Freeze the surface that was visible when the close started (`closing_surface: Option<RightSurface>` on the shell, cleared when `right_tween` ends) and render it during the tween. For Files, keep the last frame but suspend images, which is the existing `suspend_file_images`. Fade content `opacity 1→0` over the first 120 ms of the close using `motion::FADE_QUICK` (open: 0→1 over 150 ms, starting 50 ms in).
- **Before:** `mocks/SS/SS-01-before.svg`
- **After:** `mocks/SS/SS-01-after.svg`
- **Effort:** S

### SS-02 — Diff rows: no word-level highlight, near-invisible tint, four redundant add/remove signals
- **Severity:** P1
- **Category:** color/contrast
- **Where:** `crates/ui/src/changes.rs:4400-4401` (`diff_row_wash` alpha 0.055), `crates/ui/src/changes.rs:4407-4438` (`diff_row_paint`), `crates/ui/src/changes.rs:81-90` (gutter 36, marker 28, accent bar 3), `crates/ui/src/changes.rs:4547-4655` (`diff_line_row`)
- **What's wrong:** (1) The codebase has no intra-line (word) diff, so a one-character change on a 120-column line highlights the whole line and nothing else (`rg intraline|word_diff` finds nothing). (2) The row tint is 5.5% alpha in both themes. In Luna Dark that is `#0a1613` (add) or `#18100f` (del) on `#0b0b0c`, about 1.1:1. In Luna Light, emerald-600 at 5.5% on white is `#f1f9f7`, which is hard to see. (3) Each changed row repeats one signal four times: a 3 px accent bar, the tint, a coloured `+`/`−` in a 28 px marker column, and coloured line numbers. The 28 px marker column is wider than a 3-digit gutter for one glyph.
- **Why it hurts the premium feel:** Reviewing agent edits is a core job here. Xcode, Tower and GitHub all show which words changed, on a tint you can actually see. The current view is quiet where it should be clear, and repeats itself everywhere else.
- **Proposed fix:** Add a word-level pass when building `DiffHighlights`: pair each Del/Add run in a hunk and diff tokens with a ≤200 col cap. Paint changed spans as rounded 2 px background runs. Raise the row wash to 0.10 in dark and 0.08 in light, and the word wash to 0.28 in dark and 0.20 in light. Keep the tint, the marker and the word highlight. Drop the 3 px bar, or keep it only at 2 px as a scrollbar-minimap echo. Narrow `MARKER_WIDTH` 28 → 16 and paint line numbers `readable_faint` on every row. Put the new alphas on `Theme` as `diff_row_alpha` and `diff_word_alpha`.
- **Before:** `mocks/SS/SS-02-before.svg`
- **After:** `mocks/SS/SS-02-after.svg`
- **Effort:** M

### SS-03 — The diff view and 12 other scroll regions have no scrollbar at all
- **Severity:** P1
- **Category:** interaction
- **Where:** Hosts of the shared overlay rail (`popover::ScrollRailHost`): only `crates/ui/src/files/tree.rs:90` and `crates/ui/src/terminal/panel.rs:1880` in this area. Bare `overflow_y_scroll()` with no indicator: `crates/ui/src/changes.rs` (virtualised diff list, no rail), `changes.rs:4274`, `changes/git_actions.rs:733,841,1034`, `changes/pull_requests.rs:469`, `automations.rs:1446,1840`, `shell/usage.rs:2751,2912`, `browser/view.rs:276`, `context_transfer.rs:677,688,718`, `project_commands.rs:596`
- **What's wrong:** A 3,000-line diff gives no sense of position or length, and you cannot grab and drag to a region. The file editor uses the input crate's own scrollbar, and the markdown preview has another (`markdown_preview.rs:1508`). That makes three different scroll indicators, and none at all on most surfaces.
- **Why it hurts the premium feel:** macOS always shows an overlay scroller while you scroll. Without one, long content feels endless and you lose your place.
- **Proposed fix:** Implement `ScrollRailHost` for `Changes`, using the uniform row heights it already computes for the fold tween. Add a `popover::scroll_region(id, host)` helper that wraps a `div().overflow_y_scroll()` with the same auto-hiding rail (6 px thumb, 2 px inset, fades out 900 ms after the last scroll), and use it for every list above. In the diff, also show add/remove ticks in the rail track (2 px marks in `diff_add`/`diff_del` at 70%).
- **Before:** `mocks/SS/SS-03-before.svg`
- **After:** `mocks/SS/SS-03-after.svg`
- **Effort:** M

### SS-04 — Usage meters: Codex is always red, the card shows "used" while the page shows "left", 0% draws a stub
- **Severity:** P1
- **Category:** color/contrast
- **Where:** `crates/ui/src/shell/usage.rs:90` (`codex_fill: theme.danger`), `usage.rs:91` (`other_fill: text_muted`), `usage.rs:1249-1259` (card bar: 4 px, r2, `max(0.012)`), `usage.rs:33-39` and `usage.rs:924-958` (page bar: 9 px, r4, fills `left`, provider tone). Shots: `shots/surfaces/ss-usage-card-dark-codex.png`, `shots/surfaces/ss-usage-card-light-opencode.png`
- **What's wrong:** In the compact card, a Codex window at 5% is drawn in danger red, and an OpenCode window at 99% is drawn in neutral grey. Colour follows the provider, not the level. The card bar fills by percent used, while the Usage page bar (same data) fills by percent left in the provider's brand tone, so a full bar means "nothing left" in one place and "plenty left" in the other. A 0% window still paints a 1.2% nub; in the light shot it reads as a dark tick on a track you can barely see (`#e8edf3` on `#e9ebef`). The two meters also look different: 4 px / r2 against 9 px / r4.
- **Why it hurts the premium feel:** A permanently red meter trains users to ignore red. Apple's battery and storage meters use one recipe, in one direction, and change colour only at thresholds.
- **Proposed fix:** Add one `usage_meter(used_pct, theme)` helper and use it on both surfaces. Fill = used, height 6 px, `rounded_full`. Neutral fill `theme.text_muted` below 75%, `theme.warning` from 75%, `theme.danger` from 90%. Track `ink(0.08)` dark / `ink(0.10)` light. Draw no fill below 0.5% (no `max(0.012)`). Keep the provider identity in the tab icon, not the bar colour.
- **Before:** `mocks/SS/SS-04-before.svg` (+ `shots/surfaces/ss-usage-card-light-opencode.png`)
- **After:** `mocks/SS/SS-04-after.svg`
- **Effort:** S

### SS-05 — Terminal trackpad scrolling drops small deltas and jumps in whole lines
- **Severity:** P1
- **Category:** motion
- **Where:** `crates/ui/src/terminal/panel.rs:1992-2007` (`f32::from(delta.y) / line_h` then `lines.round() as i32`), `terminal/panel.rs:1405-1420` (`scroll_active` ignores 0)
- **What's wrong:** Each `ScrollDelta::Pixels` event is converted to lines and rounded on its own, and the remainder is thrown away. With an 18 px line, any trackpad event under 9 px rounds to 0, so a slow two-finger swipe does not move the terminal at all. Faster swipes move in 18 px jumps, and inertial momentum dies as soon as its per-frame deltas drop under half a line.
- **Why it hurts the premium feel:** Scrolling is the most physical interaction on a Mac. A terminal that ignores gentle swipes and then lurches feels broken next to the smooth transcript beside it.
- **Proposed fix:** Keep `scroll_remainder: f32` per tab. Add the pixel delta, emit `trunc(remainder / line_h)` lines, keep the fraction, and reset it on direction change or `TouchPhase::Ended` plus 150 ms. Optionally offset the grid by the sub-line remainder (−line_h…0) during momentum so movement is pixel-smooth, and snap to whole lines when the gesture ends.
- **Before:** `mocks/SS/SS-05-before.svg`
- **After:** `mocks/SS/SS-05-after.svg`
- **Effort:** S

### SS-06 — The right pane's toolbar row sits about 6.5 pt above the canvas tab strip
- **Severity:** P1
- **Category:** alignment
- **Where:** `crates/ui/src/shell.rs:13183` (`.pt(px(Theme::TITLEBAR_HEIGHT))` = 38), `crates/ui/src/surface_chrome.rs:7,34-43` (toolbar 38 tall), `crates/ui/src/shell/workbench/render.rs:13-15,26-32` (strip 38 inside a canvas inset to y = 44). Shot: `shots/surfaces/ss-header-band-misaligned-dark.png` (from `chat-workbench-fixture` w02)
- **What's wrong:** The `surface_chrome::toolbar` doc says the row "matches the workbench tab strip so both sidebars' second rows line up with the canvas". In the render, the right pane's toolbar text spans y 104–121 @2x (centre 56.3 pt). The canvas tab strip text spans y 117–134 @2x (centre 62.8 pt). The left sidebar's "All projects" row sits at 61 pt. The right pane starts its chrome at the bare titlebar height and ignores the canvas's top offset (`CANVAS_INSET`/`VIEWPORT_INSET`, 6 pt).
- **Why it hurts the premium feel:** The second row is one horizontal band across all three columns. When it steps 6 pt at the pane seam, the window looks assembled from parts.
- **Proposed fix:** Export the canvas's strip origin from `workbench/render.rs` (`pub const STRIP_TOP: f32 = Theme::TITLEBAR_HEIGHT + VIEWPORT_INSET` = 44). Use it for the right panel's top padding (`.pt(px(STRIP_TOP))`) so the toolbar centre lands at 63 pt. Then assert both centres match in `chat-workbench-fixture` from the readback `tabBounds` and a new `rightToolbarBounds`.
- **Before:** `mocks/SS/SS-06-before.svg` (+ `shots/surfaces/ss-header-band-misaligned-dark.png`)
- **After:** `mocks/SS/SS-06-after.svg`
- **Effort:** S

### SS-07 — Three different tab recipes across canvas, right pane and terminal drawer
- **Severity:** P1
- **Category:** consistency
- **Where:** right-pane surface chips `crates/ui/src/shell.rs:13456-13460,13624-13631,13737-13775` (24 h, r6, fixed 112 w, 11.5 px, active `wash(0.10)`); canvas tabs `crates/ui/src/shell/workbench/render.rs:15-22,1368-1385` (28 h, r7, 72–220 w, 12.5 px, active accent tint 16%); terminal drawer tabs `crates/ui/src/terminal/panel.rs:42-43,1701-1748` (bar 40 h, tab 28 h, r8, fixed 118 w, 12 px, `element_hover` blend)
- **What's wrong:** All three strips list switchable surfaces with a close glyph, but each has its own height, radius, width rule, label size and active fill. In w02, the "Related chats" chip and the "Benchmark the cache" tab sit side by side and are clearly different objects: 24 pt against 28 pt, grey against violet. The terminal drawer bar is 40 pt, where the shared strip is 38 pt.
- **Why it hurts the premium feel:** Safari, Xcode and Finder use one tab control everywhere. Three near-identical controls look like three teams built them.
- **Proposed fix:** Extract `tab_chip(id, label, icon, state)` into `surface_chrome`: 28 h, r7, 12 px label (`ui_rems(12)`), min 72 / max 220 w, active = accent wash 0.16 when the strip has focus or `wash(0.10)` without it, hover `wash(0.06)` via `motion::HOVER_FADE`. Use it for the right-pane chips and the terminal drawer. Set `TAB_BAR_HEIGHT` 40 → `surface_chrome::HEADER_HEIGHT` (38).
- **Before:** `mocks/SS/SS-07-before.svg`
- **After:** `mocks/SS/SS-07-after.svg`
- **Effort:** M

### SS-08 — No shared toast: feedback snaps into the left sidebar, one slot, four notice recipes
- **Severity:** P1
- **Category:** consistency
- **Where:** sidebar notice `crates/ui/src/shell.rs:9745-9793` (+ lifetimes `shell.rs:2069-2078`: 4 s status, 10 s error); workbench notice `crates/ui/src/shell/workbench/render.rs:1697-1790`; composer `crates/ui/src/notice.rs:9-72`; Files watch banner `crates/ui/src/files/mod.rs:274-322`
- **What's wrong:** Most feedback goes to `sidebar_notice`. It is mounted with `.when_some(...)`, so it pops in with no transition and pushes the usage card and footer up by its height. It sits inside the left sidebar column, so it is not visible when that sidebar is hidden, even though many messages come from right-pane actions ("That chat isn't available here.", workbench "no room"). It holds one message: a new one replaces the old. "Dismiss" is bare medium text with no hover. Four recipes exist for one job: sidebar (r6, 11 px, accent wash, no icon), workbench (r8, 12 px, raised, danger border 0.5, ✕ 18 px), composer chip (r10, 12 px, triangle icon, copy 20 px), Files banner (27 h band, 10 px text, 10.5 px icon).
- **Why it hurts the premium feel:** macOS gives transient feedback one consistent, animated home. Here feedback appears in different places and styles, and sometimes off-screen.
- **Proposed fix:** Add a `toast` module: a window-level stack anchored bottom-centre of the canvas, 12 pt above the composer, max 3 visible. Each toast is a 36 h capsule (`rounded_full`), `glass_overlay` background, 1 px `hairline(0.10)`, shadow 0 8 24 rgba(0,0,0,0.35), 16 px tone icon and 12.5 px text. Enter: fade plus 8 pt rise plus scale 0.96→1 over 220 ms `EASE_OUT_EXPO`; exit: fade plus 4 pt drop over 160 ms. Keep the existing lifetimes, pause the timer on hover, and allow an optional single action. Route `sidebar_notice` and the workbench notice through it. Keep `notice_chip` only for in-context composer errors.
- **Before:** `mocks/SS/SS-08-before.svg`
- **After:** `mocks/SS/SS-08-after.svg`
- **Effort:** M

### SS-09 — The image lightbox snaps open and closed with no zoom origin, controls or readout
- **Severity:** P1
- **Category:** motion
- **Where:** `crates/ui/src/attachments.rs:947-1034` (`lightbox_with_size`: `deferred(anchored(...))` with flat `scrim_alpha(0.7)`, caption 11 px at `ink(0.45)`), `crates/ui/src/image_viewer.rs:43-152` (fit/zoom/pinch only), `crates/ui/src/files/image_preview.rs:199-214`
- **What's wrong:** Opening an image mounts the full-window scrim and the fitted image in one frame, and Escape or a click removes them in one frame. There is no zoom from the thumbnail, no fade, and no scale. Inside the viewer there is no visible close button, no zoom percentage, and no double-click to switch between fit and 100%. The Files image preview has no checkerboard behind transparent PNGs. "Loading image…" is plain text in both places.
- **Why it hurts the premium feel:** Quick Look and Photos grow the image out of its thumbnail and back. An instant black flash feels like a page reload.
- **Proposed fix:** Pass the thumbnail's bounds into `PreviewImage`. Animate scrim alpha 0→0.7 and image frame from the thumbnail rect to the fitted rect over 280 ms (`EASE_OUT_EXPO`); close in reverse over 200 ms. Add a 28 h glass capsule top-right with ✕, a zoom % readout and "Fit". Double-click toggles fit ↔ 100% at the cursor with a 220 ms spring. Paint an 8 pt checkerboard (`ink(0.04)`/`ink(0.08)`) under images that have alpha.
- **Before:** `mocks/SS/SS-09-before.svg`
- **After:** `mocks/SS/SS-09-after.svg`
- **Effort:** M

### SS-10 — File tree selection is a full-bleed band; focus and blur look the same; no indent guides
- **Severity:** P2
- **Category:** consistency
- **Where:** `crates/ui/src/files/tree.rs:18-19` (row 27, indent 14), `tree.rs:223-237` (`w_full`, `pl(8 + depth·14)`, selection `wash(0.12)` focused / `wash(0.08)` unfocused, no radius), `crates/ui/src/files/search.rs:24,546` (search selection `wash(0.10)`)
- **What's wrong:** The selected row is a square, edge-to-edge band, while the left sidebar's selected chat is an inset rounded card (8 pt inset, about 10 pt radius, visible in every shot). Focused (0.12) and unfocused (0.08) selection differ by 4% white and are hard to tell apart, so you cannot see whether the tree has keyboard focus. Search results use a third value (0.10). Deep folders have no indent guides, so at depth 4+ siblings are hard to match up. Rows are 27 pt, between Xcode's 22–24 pt navigator rows and Finder's 28 pt sidebar.
- **Why it hurts the premium feel:** Xcode and Finder use an inset pill selection, accent-tinted when focused and grey when not. Every other list in Luna already does something closer to that.
- **Proposed fix:** Inset the row content 6 pt left and right and use `rounded(px(6))`. Focused selection = `accent.opacity(0.22)` with `text` label; unfocused = `wash(0.08)`. Use the same token for search rows. Add 1 px `hairline(0.06)` indent guides at each ancestor's chevron centre (x = 8 + 7 + depth·14), brightened to 0.12 for the selected row's ancestors. Row height 26 pt (a 2 pt grid with 14 pt icon plus 6 pt padding), and update `TREE_ROW_HEIGHT` and `SEARCH_ROW_HEIGHT` together.
- **Before:** `mocks/SS/SS-10-before.svg`
- **After:** `mocks/SS/SS-10-after.svg`
- **Effort:** S

### SS-11 — Disclosure chevrons behave three different ways
- **Severity:** P2
- **Category:** motion
- **Where:** Files tree `crates/ui/src/files/tree.rs:266-283` (11 px glyph swap, instant); diff file header `crates/ui/src/changes.rs:3600-3626` (13 px glyph swap + opacity 0.25→1 over `CHEVRON` 200 ms); stale comment `crates/ui/src/motion.rs:393-395` ("gpui divs have no rotation transform"); real rotation already used in `crates/ui/src/shell/spaces.rs:648-668` (12 px, rotate 0→90° over `COLLAPSE` 180 ms) and `crates/ui/src/transcript.rs:7986-7993` (14 px)
- **What's wrong:** Expanding a folder swaps `ALT_ARROW_RIGHT` for `ALT_ARROW_DOWN` in a single frame. Expanding a diff file swaps the glyph and fades it up from 25%, so it blinks. The sidebar and transcript rotate one glyph smoothly. The rotation API exists (`Transformation::rotate`), so the `CHEVRON` doc comment is out of date. Sizes vary too (11 / 12 / 13 / 14 px) and so do colours (`text_faint`, `readable_muted(0.7)`, `text_muted`).
- **Why it hurts the premium feel:** Disclosure triangles are a signature macOS control. One that rotates in one pane and blinks in the next makes the app feel uneven.
- **Proposed fix:** Add one `disclosure_chevron(id, open_progress, theme)` helper: a single `ALT_ARROW_RIGHT` at 12 px in `readable_faint(0.8)`, rotated `progress·90°`, animated with `motion::COLLAPSE` (180 ms ease-out). Use it in the tree, diff headers, spaces and transcript. Fix the `CHEVRON` comment, or delete `CHEVRON` in favour of `COLLAPSE`.
- **Before:** `mocks/SS/SS-11-before.svg`
- **After:** `mocks/SS/SS-11-after.svg`
- **Effort:** S

### SS-12 — Empty, loading and error states: six recipes, one spinner, one blank pane
- **Severity:** P2
- **Category:** states
- **Where:** Files root loading = `div().flex_1()` blank `crates/ui/src/files/mod.rs:263-269`; Files error 12 px text + Retry 28 h / r7 `files/mod.rs:215-262`; file preview `centered_state` 11.5 px `crates/ui/src/files/preview.rs:3455-3467`; Diff spinner + "Preparing diff…" 12 px `crates/ui/src/changes.rs:5095-5106`; Browser icon tile 44/r12 + 14 px title + 28 h / r6 button `crates/ui/src/browser/view.rs:285-395`; Terminal 12 px faint line `crates/ui/src/terminal/panel.rs:1937-1950`; "Loading…" text in `files/tree.rs:304` (10.5 px), `automations.rs:1506` (11 px), `files/image_preview.rs:213` (12 px), `changes/git_actions.rs:889`, `shell/usage.rs:2718`
- **What's wrong:** Opening Files on a slow remote shows an empty pane with nothing to say it is loading. Only the Diff uses `loaders::gradient_spinner`; seven other surfaces print "Loading…" in four sizes. Empty states range from a full Browser hero (icon tile, title, body, button) to a single faint line in the Terminal. Action buttons in these states are 28 h at r7, r6 and r6+border, so none of them match the 24 h / r6 `surface_chrome` control.
- **Why it hurts the premium feel:** Apple's empty states are one recognisable layout: glyph, title, one line, one button. Mixed states make every pane feel like a different app.
- **Proposed fix:** Add `surface_state(kind: Loading|Empty|Error, glyph, title, body, action)` to `surface_chrome`: 32 px glyph in `text_faint`, 13 px medium title, 12 px / 18 lh body in `text_muted` (max 280 w), optional 28 h / r7 bordered button. Loading uses `gradient_spinner(16)` plus a 12 px label after 150 ms (no flash for fast loads). Use it on all eight surfaces; tree-level "Loading…" rows keep row form but use `mini_mono_spinner`.
- **Before:** `mocks/SS/SS-12-before.svg`
- **After:** `mocks/SS/SS-12-after.svg`
- **Effort:** M

### SS-13 — Git card: four identical buttons, no primary action, a toggle with no state
- **Severity:** P2
- **Category:** interaction
- **Where:** `crates/ui/src/changes/git_actions.rs:669-688` (`git_button`: padding 8/5, r6, `surface_raised` + border, 11 px), `git_actions.rs:848-883` (Generate message · Commit · Push · Commit & Push), `crates/ui/src/changes.rs:4033-4049` ("Git" toggle: 24 h, `wash(0.05)`, no hover, no active latch), `changes.rs:5166-5170` (card mounts with no transition)
- **What's wrong:** The commit row shows four equal-weight bordered buttons, so the irreversible "Commit & Push" looks the same as "Generate message". Commit is not disabled when nothing is selected or the message is empty. The toolbar's "Git" text button has no hover and no latched look while the card is open, unlike the split and wrap toggles beside it (`header_toggle` holds `wash(0.14)`). The card then snaps in and pushes the diff down.
- **Why it hurts the premium feel:** Mac sheets show one default (accent) button and quieter secondaries. Equal weight makes the user read every label before acting.
- **Proposed fix:** Make "Commit" the default button: accent fill, `accent_text` label, 26 h, r7. Show "Commit & Push" as a split-button chevron menu on it (or a secondary). "Push" becomes secondary (bordered), and "Generate" becomes an icon button (sparkle, 24 h) inside the message field. Disable Commit (opacity 0.4, no pointer) when nothing is selected or the message is empty. Turn the toolbar "Git" into `header_toggle` with a branch icon so it latches. Reveal the card with `COLLAPSE` (180 ms height plus fade).
- **Before:** `mocks/SS/SS-13-before.svg`
- **After:** `mocks/SS/SS-13-after.svg`
- **Effort:** M

### SS-14 — Pane content does not share the toolbar's left edge (8 vs 12 vs 16 vs 20 pt)
- **Severity:** P2
- **Category:** alignment
- **Where:** toolbar edge `crates/ui/src/surface_chrome.rs:12` (`EDGE_INSET` 8); diff file header `crates/ui/src/changes.rs:3651` (`SPACE_MD` 12); hunk header `changes.rs:4479` (16); Agents body `crates/ui/src/shell/agents.rs:41` (12) and empty `agents.rs:58` (20); Automations body `crates/ui/src/automations.rs:1446` (12); Browser previews `browser/view.rs:242,277` (16); Files error `files/mod.rs:224` (28). Shot: `shots/surfaces/ss-agents-surface-dark.png` ("Agents · this conversation" at x 1856 @2x, rows at x 1863 @2x)
- **What's wrong:** Each pane title sits at 8 pt from the pane edge, while the first content column sits at 12, 16 or 20 pt depending on the surface. The Agents render shows a visible 3.5–4 pt step between header and rows. In the Diff, the toolbar, file header and hunk header use three different left edges (8 / 12 / 16).
- **Why it hurts the premium feel:** A shared leading edge is the cheapest way to make a pane look designed. Ragged edges look unfinished even when you can't say why.
- **Proposed fix:** Add `surface_chrome::CONTENT_INSET = 12.0` and set toolbar padding to `px(CONTENT_INSET - 4)` so that icon buttons, which carry 4 pt of internal air, put their glyph on 12 pt and text titles start at 12 pt. Then make the content of every surface start at 12 pt (diff headers, hunk header, agents, automations, previews, empty states).
- **Before:** `mocks/SS/SS-14-before.svg` (+ `shots/surfaces/ss-agents-surface-dark.png`)
- **After:** `mocks/SS/SS-14-after.svg`
- **Effort:** S

### SS-15 — Agents surface: a wall of engine diagnostics with two row recipes in one pane
- **Severity:** P2
- **Category:** states
- **Where:** `crates/ui/src/shell/agents.rs:17-160`, `crates/ui/src/task_work.rs` (work rows), engine copy `crates/engine/src/rpc/task_work.rs:457,492`, workflow cards `crates/ui/src/claude_advanced.rs:141-205`. Shot: `shots/surfaces/ss-agents-surface-dark.png` (`native-agents-fixture` 01)
- **What's wrong:** Every work row prints its state as text ("Completed · Subagent · …") followed by the engine's `detail` string, which is the same diagnostic on almost every row: "Provider-observed lifecycle; stop acknowledgement alone is not terminal evidence". In the fixture that wraps to two lines on five consecutive rows. There is no status glyph, so a Failed row looks like a Completed one. Below it, "Workflows & teammates" uses bordered r8 cards with a 7 px status dot, so one pane holds two different list designs. Type uses 10 / 11 / 11.5 px plus an inherited size.
- **Why it hurts the premium feel:** Engineering language shown to users, with no visual hierarchy, makes the surface read like a debug panel rather than an Activity view.
- **Proposed fix:** Use one row recipe for both lists: 7 px status dot (success / danger / warning / faint) plus 13 px title, then an 11.5 px `text_muted` meta line ("Subagent · 9 s · 2.4k tokens"). Move the engine `detail` to the row tooltip, or show it only when it differs from the default lifecycle copy. Group the sections under 11 px uppercase `text_faint` headers. Show Failed rows with a danger dot and title, and keep the copy neutral.
- **Before:** `mocks/SS/SS-15-before.svg` (+ `shots/surfaces/ss-agents-surface-dark.png`)
- **After:** `mocks/SS/SS-15-after.svg`
- **Effort:** M

### SS-16 — Keyboard focus is invisible on most secondary-surface controls
- **Severity:** P2
- **Category:** interaction
- **Where:** 67 `cursor_pointer()` controls in this area against 16 with a focus style (`focus_visible` ×5: `changes/git_actions.rs:686`, `files/preview.rs`×2, `project_commands.rs`×2; `accessible_button` ×11: `files/mod.rs:235`, `automations.rs`×10). Shared helpers without focus: `crates/ui/src/files/mod.rs:44-64` (`toolbar_button`, also used by Browser), `changes.rs:3781-3826` (`header_toggle`), the right-surface chips. Three focus looks: inset 2 px accent ring (`theme::focus_ring`), `border_color(accent)` (git), `border_color(border_strong)` (surface picker)
- **What's wrong:** Tabbing through the Files, Diff, Browser or Usage toolbars moves focus with no visible indicator. Where focus is drawn, it uses one of three different looks.
- **Why it hurts the premium feel:** macOS draws one recognisable focus ring. Without it, keyboard users are lost, and the inconsistency is noticeable even to mouse users who tab occasionally.
- **Proposed fix:** Add `.focus_visible(|s| theme::focus_ring(s, theme.focus_ring()))` inside `toolbar_button`, `header_toggle`, `git_button`, the right-surface chip and the usage provider tabs. Then delete the border-colour variants. Spread stays 2 px inset; use the radius of the control.
- **Before:** `mocks/SS/SS-16-before.svg`
- **After:** `mocks/SS/SS-16-after.svg`
- **Effort:** S

### SS-17 — Type and radius sprawl: 15 text sizes, 9 radii, px line-heights under rem text
- **Severity:** P2
- **Category:** typography
- **Where:** area-wide count over the files in scope: `ui_rems` sizes 9, 10, 10.5, 11 (99 uses), 11.5, 12, 13, 14, 15, 16, 17, 19, 29, 30, 36; literal radii 2, 3, 4, 5, 6, 7, 8, 10, 12. Fixed `line_height(px(16.0))` under `ui_rems(11.0)` at `crates/ui/src/automations.rs:1423,1542,1673,1832,1971` and `crates/ui/src/context_transfer.rs:594,631,723,754`; the `small` closure is redefined at each of those sites
- **What's wrong:** Within one pane, labels step 10 → 10.5 → 11 → 11.5 → 12 px, gaps too small to read as hierarchy. Radii include 5 and 7 px beside 6, and 4 beside 3. At the 20 px UI-size setting, `ui_rems(11)` becomes 13.75 px but the px(16) line height stays, a 1.16 ratio, so descenders touch the next line in Automations and Context transfer.
- **Why it hurts the premium feel:** Apple's type ramp has few steps that are clearly different. Near-duplicates look accidental, and fixed leading under scalable text breaks the accessibility setting it exists for.
- **Proposed fix:** Map every surface to the DS ramp (proposed: caption 11 / 15, footnote 12 / 17, body 13 / 19, headline 13 semibold, title 15 / 20), with line heights in `ui_rems`. Replace 10/10.5 with caption 11, and 11.5 with footnote 12. Use three radii: 6 (controls), 8 (rows/inputs in cards), 10 (cards/panels), plus `rounded_full` for capsules. Replace the local `small` closures with a shared `text::caption()`.
- **Before:** `mocks/SS/SS-17-before.svg`
- **After:** `mocks/SS/SS-17-after.svg`
- **Effort:** M

### SS-18 — Files status text below the readable floor (10–10.5 px, 10.5 px icon)
- **Severity:** P2
- **Category:** typography
- **Where:** watch banner `crates/ui/src/files/mod.rs:274-322` (27 h, 10 px text, 10.5 px icon, 20 h "Refresh now"); tree status/error/load-more rows `crates/ui/src/files/tree.rs:300-365,478-494` (10.5 px); error row copy `tree.rs:340-343` (`"{message} — Retry"` as plain text)
- **What's wrong:** File-watch failures, "Empty folder", "Loading…", "Load more…" and per-folder errors are the smallest text in the app, smaller than the 11.5 px file names they sit beside. The error row joins the message and "Retry" into one string, so the affordance is an em dash. The banner's 10.5 px refresh glyph is under the 12 px icon minimum used elsewhere.
- **Why it hurts the premium feel:** Status text is shrunk only when it is unimportant. These are exactly the states where the user needs to read and act.
- **Proposed fix:** Status rows use 11.5 px (same as names), italic off, colour `text_faint`, and a leading 12 px glyph (spinner / folder-dashed / danger-circle) aligned in the icon column. Error rows show the message in `danger` tone plus a separate trailing "Retry" link-button (11.5 px medium, accent). The banner becomes a 32 h strip with 12 px text, a 14 px icon and a 24 h r6 "Refresh" control (`toolbar_button` style).
- **Before:** `mocks/SS/SS-18-before.svg`
- **After:** `mocks/SS/SS-18-after.svg`
- **Effort:** S

### SS-19 — Editor breadcrumb truncates every segment and buries the file name
- **Severity:** P2
- **Category:** typography
- **Where:** `crates/ui/src/files/preview.rs:2390-2420` (each crumb `min_w_0().truncate()`, 11 px; last crumb `text_muted`, others `text_faint`; "›" text separator at `readable_faint(0.65)`)
- **What's wrong:** In a 360 pt pane, `crates / ui / src / files / preview.rs` truncates every segment at once ("cra… › ui › sr… › fil… › previ…"), so the file name, the one segment that matters, is cut too. The current file is drawn in `text_muted`, only slightly brighter than its parents, so nothing anchors the eye. The diff header shows the same path as 12 px mono in `text_dim`, so the same file looks different in two panes.
- **Why it hurts the premium feel:** Xcode's jump bar and Finder's path bar always keep the leaf visible and collapse the middle segments first.
- **Proposed fix:** Give the leaf `flex_none` (truncate it only if it alone overflows) in `text` at 12 px medium. Collapse middle segments into a "…" menu button once the row overflows (measure with the existing `width_cell`). Separators become a 10 px `ALT_ARROW_RIGHT` icon in `text_faint`. Use the same leaf-emphasised path component in the diff file header.
- **Before:** `mocks/SS/SS-19-before.svg`
- **After:** `mocks/SS/SS-19-after.svg`
- **Effort:** S

### SS-20 — Diff hunk headers sit on their own grid and scale with the wrong setting
- **Severity:** P3
- **Category:** alignment
- **Where:** `crates/ui/src/changes.rs:4472-4486` (`hunk_header_row`: 28 h, `px(SPACE_LG)` = 16 left, `ui_rems(11.0)` mono), vs code text `changes.rs:4508-4511` (`px(diff_text_size)` 12, code starts at 3 + 2·36 + 28 + 12 = 115 pt)
- **What's wrong:** The `@@ -12,7 +12,9 @@` text starts at 16 pt, unrelated to the line-number gutters (3–75 pt) or the code column (115 pt). It also uses `ui_rems`, so the UI font size moves it while the code-font setting moves the code. At UI 20 px / code 12 px, the hunk header (13.75 px) is larger than the code it introduces.
- **Why it hurts the premium feel:** Section headers that ignore the columns under them make the diff look loosely assembled.
- **Proposed fix:** Size hunk text from `diff_text_size(theme) · 11/12` (code-font driven, like the gutters). Start the range at the gutter column, right-aligned like the numbers, and the function context (`fn render_row`) at the code column in `text_muted`. Drop the 28 h band to `diff_line_height + 6`, with an 8 pt top margin before every hunk except the first.
- **Before:** `mocks/SS/SS-20-before.svg`
- **After:** `mocks/SS/SS-20-after.svg`
- **Effort:** S

### SS-21 — Browser toolbar: no load progress, Reload never becomes Stop, the error border shifts the field
- **Severity:** P3
- **Category:** states
- **Where:** `crates/ui/src/browser/view.rs:404-512` (toolbar), `browser/view.rs:443-448` (`border_1()` added only on validation error), `browser/view.rs:472-494` (Go button 18 / r4 inside a 24 / r6 field); `page.loading` (`browser/model.rs:8`) is read only for the tab indicator at `crates/ui/src/shell.rs:13562-13565`
- **What's wrong:** While a page loads, the address field and Reload look exactly as they do at rest. The only signal is a small indicator on the surface tab. On an invalid URL the field gains a 1 px border, which nudges the text 1 pt right and down. The Go button's 18 / r4 square inside a 24 / r6 field leaves 3 pt above and below with corners that don't nest (r4 inside r6 with 3 pt inset should be r3).
- **Why it hurts the premium feel:** Safari's address bar fills with a thin progress line and Reload turns into Stop. Small details like these are what make a browser feel native.
- **Proposed fix:** Draw a 2 px accent progress line along the field's bottom inner edge while `page.loading` (indeterminate 1.2 s shimmer if no estimate). Swap Reload ↔ Stop (`CLOSE` glyph) while loading. Always reserve the border: `border_1().border_color(transparent)` at rest, `danger` on error. Go button 18 h, `rounded(px(3))`.
- **Before:** `mocks/SS/SS-21-before.svg`
- **After:** `mocks/SS/SS-21-after.svg`
- **Effort:** S

---

## Top 5 fixes with the biggest premium payoff

1. **SS-02 — Word-level diff highlights plus a visible tint.** The diff is where users judge agent output. Clear changed words on a 10% row tint is the biggest quality jump in this area.
2. **SS-01 + SS-09 — Motion that carries content.** Freeze the right pane's surface while it closes and grow images out of their thumbnails. These two fix the most visible "snap" moments.
3. **SS-08 — One animated toast stack.** It replaces four notice designs and stops feedback from landing in a hidden sidebar.
4. **SS-07 + SS-06 — One tab chip and one aligned second row.** Three tab recipes and a 6.5 pt step at the pane seam become one continuous band across the window.
5. **SS-03 + SS-05 — Scroll you can feel and see.** Add the shared overlay scrollbar to the diff and every list, and make terminal scrolling pixel-smooth.
