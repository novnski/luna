# App shell, chrome and navigation (SH)

## Area overview

The shell is well engineered. Hit-testing, drag regions, tween reversal, reduced motion and
contrast flooring are all handled with care. The visual system, though, was assembled one user
report at a time, and it shows at the seams. In one window there are two tab systems, two
splitter styles, two tooltip styles and four focus-ring recipes. Chrome controls come in eight
sizes and their icons in eight sizes. Shell text uses fifteen font sizes, 46 of them under 11 px.
Hover handling mixes 48 instant `.hover()` calls with 10 faded ones, across seven wash alphas.
The most visible daily problems:

- The selected sidebar row paints exactly the hover fill.
- The tab strip hard-clips tabs, and Main can scroll out of view.
- The command palette re-centres as you type, and it appears and disappears without motion.
- The titlebar repeats the tab's title on a different left edge.
- In light mode, the content plane is darker than the chrome, and disabled Back/Forward look enabled.

One composer notice is unreadable when content scrolls under it (P0). Navigation (switching
chats, collapsing the sidebar) snaps or wipes instead of moving like macOS.

**Evidence.** Headless production renders came from `startup-fixture` (cases `empty`, `active`,
`mixed-active`), `chat-workbench-fixture` (44 frames; it stopped at a later split-control step,
which did not affect the captured frames), `related-chats-fixture`, `chat-placement-fixture`,
`side-chat-pane-fixture`, `child-chats-pane-fixture` (picker frame only) and
`empty-project-fixture`. The captures are translucent (the frost alpha is baked in). They were
composited over a neutral desktop: `#26262C` for dark, `#ECECF0` for light. Crops are in
`shots/shell/`. Measurements are in points (1×). Code references are against `590cf4b6`.

---

### SH-01 — Composer notice is a 5% tint, so transcript text shows straight through it
- **Severity:** P0
- **Category:** color/contrast
- **Where:** `crates/ui/src/notice.rs:26-29` (plate), caller `crates/ui/src/composer.rs:12441`; `chat-workbench-fixture` frame `w16-empty-group` (`shots/shell/notice-overlaps-transcript.png`)
- **What's wrong:** `notice_chip` paints `bg(accent.opacity(0.05))` with a `0.16` border and no blur. It floats in the composer dock over the transcript. In the fixture frame, the user bubble "Check the release notes" sits under "…delivery receipt; it was preserved and will…". The two lines of 12 px text overprint each other. The composer pill right below is frosted. The notice above it is not.
- **Why it hurts the premium feel:** Text colliding with text looks broken. It also lands at the worst moment: when the app is telling you a message may not have been delivered.
- **Proposed fix:** Give the notice an opaque plate: warning 8% flattened over `theme.canvas_fill()` (≈`#17140A` in Luna Dark), with a 1 px warning @ 22% ring. Alternatively, wrap it in `crate::frost::frosted(10.0, MENU_BLUR, …)` with the composer's tint. Also add the 24 px `TRANSCRIPT_FADE_BAND` above the whole dock stack so rows dissolve before they reach it. Keep the notice inside the measured composer height used by `pane_chat.rs:822-832` and `shell.rs:15164`.
- **Before:** `mocks/SH/SH-01-before.svg`, `shots/shell/notice-overlaps-transcript.png`
- **After:** `mocks/SH/SH-01-after.svg`
- **Effort:** S

### SH-02 — The selected sidebar row paints exactly the hover fill
- **Severity:** P1
- **Category:** states
- **Where:** `crates/ui/src/theme.rs:2297-2302` (`glass_selected_bg`), `theme.rs:1601` / `theme.rs:1689` (`element_hover`), `crates/ui/src/shell.rs:9060-9085` (row fill, "No selection ring"), `crates/ui/src/shell/spaces.rs:2501-2502` (shelf rows); `shots/shell/sidebar-selected-row-dark.png`
- **What's wrong:** In dark, selected = `wash(0.11)` = `hsla(0,0,0.92,0.11)` and hover = `element_hover` = `hsla(0,0,0.92,0.11)`. In light, both are `hsla(0,0,0.10,0.06)`. Selection has no ring. So the hovered row and the open chat look identical. With the pointer resting anywhere in the sidebar there appear to be two selected chats. The light selected fill is only 1.12:1 against the chrome.
- **Why it hurts the premium feel:** Finder and Mail always make "where you are" unambiguous. Here the key navigation state disappears under the pointer.
- **Proposed fix:** Selected = `wash(0.16)` dark / `wash(0.10)` light, plus a 1 px inset hairline (`card_selected_shadows`, 9% / 7%). Hover drops to `wash(0.06)` / `wash(0.04)`, faded over `HOVER_FADE`. Optionally use an accent-tinted selection (`accent @ 0.18`) while the window is key, and the neutral fill when it is not, the way Finder does. Apply the same tokens to settled and archived rows.
- **Before:** `mocks/SH/SH-02-before.svg`
- **After:** `mocks/SH/SH-02-after.svg`
- **Effort:** S

### SH-03 — The tab strip hard-clips tabs, and Main can scroll out of view
- **Severity:** P1
- **Category:** interaction
- **Where:** `crates/ui/src/shell/workbench/render.rs:1155-1167` (tab scroller, `overflow_x_scroll` with no fade); `chat-workbench-fixture` readback and frames `w07a-chooser`, `w15-narrow`; `related-chats-fixture` `r11`
- **What's wrong:** Tabs scroll in a plain `overflow_x_scroll` container. There is no edge fade, no ellipsis on the clipped tab and no overflow menu. "Draft rele" is cut mid-glyph, flush against the `+` button. The readback shows the Main tab at `x = −69.5` with a visible width of `0`: the workbench's anchor chat is scrolled completely out of sight, and only a 4 px sliver of its accent badge remains. `overflowFades` reports all `false`.
- **Why it hurts the premium feel:** Sliced glyphs read as a layout bug. Losing Main breaks the workbench's mental model, because Main is the one tab that can never be closed.
- **Proposed fix:** (1) Pin Main outside the scroller as a compact chip (brand mark + Main badge) with a 1 px separator. (2) Wrap the scroller in `edge_fade::edge_faded(24, …)` with `fade_overflow_x`, the same helper and band the sidebar uses. (3) Add a `+N ⌄` overflow button that lists hidden tabs. (4) `scroll_to_item` the active tab on focus changes.
- **Before:** `mocks/SH/SH-03-before.svg`, `shots/shell/tab-strip-hard-clip.png`, `shots/shell/tab-strip-main-scrolled-away.png`
- **After:** `mocks/SH/SH-03-after.svg`
- **Effort:** M

### SH-04 — The command palette re-centres while you type, and appears in a single frame
- **Severity:** P1
- **Category:** motion
- **Where:** `crates/ui/src/shell/command_palette.rs:1155-1162` (scrim with `items_center` + `justify_center`), `command_palette.rs:1028-1032` (body `max_h` 100–360), `crates/ui/src/motion.rs:374-377` (`MENU_IN` / `MENU_OUT` unused here)
- **What's wrong:** The card is flex-centred in the viewport, and its height follows the result count. In an 800 px window, the search field's top moves from 219 (9 rows) to 309 ("pol", 3 rows) to 321 ("No results") while you type. The field you are typing into jumps about 100 px. The card and its 35% scrim appear and disappear in one frame. No fade, scale or exit motion is used, although `MENU_IN` (140 ms) and `DIALOG_IN` (180 ms) exist.
- **Why it hurts the premium feel:** Spotlight and Raycast pin the field and grow results downward. A moving input is the opposite of calm, and an un-animated summon feels like a web modal.
- **Proposed fix:** Anchor the card's top at `max(88, 0.2 × viewport_h)` (`items_start` + `pt`) and let the body grow down to 360. Entrance: 160 ms, opacity 0→1, scale 0.97→1, y +6→0, curve `cubic-bezier(.2,.9,.3,1)`; scrim 0→35% over 120 ms. Exit: opacity only, 100 ms (`MENU_OUT`). Reduced motion: no transform.
- **Before:** `mocks/SH/SH-04-before.svg`
- **After:** `mocks/SH/SH-04-after.svg`
- **Effort:** S

### SH-05 — The titlebar repeats the tab's title, on a different left edge
- **Severity:** P1
- **Category:** consistency
- **Where:** `crates/ui/src/shell/tabs.rs:271-525` (`render_session_title_bar`; title at `:494-506`, target at `:508-516`), `content_left` at `tabs.rs:341-342`; `crates/ui/src/theme.rs:1288` (`TITLEBAR_HEIGHT` 38); frame `w01-main` (`shots/shell/titlebar-title-duplicates-tab.png`)
- **What's wrong:** With the workbench showing, the 38 px titlebar shows the chat title (12 px Medium @ 85%, smaller than the tab's 12.5 px) plus "project @ device". The Main tab 46 px below repeats the same title. On narrow windows the titlebar keeps the full long title while the tab is crushed to "Release plan for the…". Measured left edges, stacked vertically: canvas 264, titlebar glyph 272, tab chip 276, tab glyph 285. Nothing shares a column.
- **Why it hurts the premium feel:** Duplicate identity is noise, and four near-miss left edges read as "almost aligned", the clearest tell of an unpolished layout. Mail and Safari show the title once and align the toolbar with the content below it.
- **Proposed fix:** In workbench mode, the titlebar carries context only: project at 13/600, then "›", then device at 13 muted, then branch. The chat title lives in the tab. Put the titlebar text on the first tab glyph's column (x = canvas left + 21 = 285). Keep showing the title in the titlebar only on the non-workbench route. Optionally move to a 52 pt unified toolbar (traffic-light centre y 26) if a full toolbar row is wanted later; with context-only content, 38 pt compact is fine.
- **Before:** `mocks/SH/SH-05-before.svg`, `shots/shell/titlebar-title-duplicates-tab.png`
- **After:** `mocks/SH/SH-05-after.svg`
- **Effort:** S

### SH-06 — Two tab systems in one window, and their header bands sit 6 px apart
- **Severity:** P1
- **Category:** consistency
- **Where:** Workbench tabs: `crates/ui/src/shell/workbench/render.rs:15-22`, `:1366-1391` (28 px, r7, 12.5 px, trailing close, accent 0.16 active). Right-pane tabs: `crates/ui/src/shell.rs:13591-13626`, `:13737` (24 px, r6, 11.5 px, close swaps into the icon slot, wash 0.10 active). `crates/ui/src/surface_chrome.rs:7`; frame `w04-nested-splits` (`shots/shell/two-tab-systems.png`)
- **What's wrong:** The two tab strips differ in height, radius, type size, close position and active fill. The right pane's tabs also live in the titlebar band, while workbench tabs live inside the canvas. Measured text centres of the two "second rows": workbench strip y = 62.75, right-pane toolbar y = 57. The comment at `render.rs:11-14` says they "read as one aligned band", but they miss by about 6 px. The workbench strip sits inside the canvas (38 + 1 border + 5 viewport inset), while the right toolbar starts at 38.
- **Why it hurts the premium feel:** The same concept drawn two ways makes the app feel like two products bolted together. The 6 px stagger is visible across the full window width.
- **Proposed fix:** Extract one `TabChip`: 28 px, r8, 12.5 px (Medium only via colour, see SH-22), trailing 16 px close that fades in over 120 ms, active `tab_focused_fill` / `tab_chip_washes()[2]`. Use it for both strips. Put the right pane's strip on the same plane and offset as the canvas strip (see SH-23), so both second rows centre on y = 63.
- **Before:** `mocks/SH/SH-06-before.svg`, `shots/shell/two-tab-systems.png`
- **After:** `mocks/SH/SH-06-after.svg`
- **Effort:** M

### SH-07 — Light mode: the content plane is a grey tray, darker than the chrome
- **Severity:** P1
- **Category:** color/contrast
- **Where:** `crates/ui/src/theme.rs:1096-1102` (`canvas_fill` → `ink(0.035)` in light), `crates/ui/src/shell/workbench/render.rs:142-183` (`contained_plane`); `empty-project-fixture` `light-rest`, `chat-workbench-fixture` `w14-light`
- **What's wrong:** Composited, the light canvas measures (218, 220, 224). The chrome around it measures (226, 228, 232), so the reading surface is darker than the sidebar (1.08:1). The theme's own doc comment says "content plane white, chrome grey". The canvas recipe inverts that.
- **Why it hurts the premium feel:** Every Apple light-mode app puts content on white paper inside grey frosted chrome. A recessed grey tray makes light mode look dim and second-class.
- **Proposed fix:** `canvas_fill(light)` = `self.bg` (white) at ≥ 0.96 over the frost. Keep the 10% hairline and add a soft seat: `0 2px 8px rgba(0,0,0,.06)`. Re-check `contrast_checked_glass_alpha` inputs (the canvas entry). Dark is unchanged.
- **Before:** `mocks/SH/SH-07-before.svg`, `shots/shell/light-canvas-recessed.png`, `shots/shell/light-workbench.png`
- **After:** `mocks/SH/SH-07-after.svg`
- **Effort:** S

### SH-08 — Light mode: disabled Back/Forward look exactly like enabled controls
- **Severity:** P1
- **Category:** states
- **Where:** `crates/ui/src/theme.rs:1077-1083` (`decorative_muted` returns `text_muted` at 100% in light), `crates/ui/src/shell.rs:14640-14668` (`nav_history_button`); `empty-project-fixture` `light-rest` vs `dark-rest`
- **What's wrong:** Measured darkest glyph luminance in light: sidebar toggle 79, disabled Back 79, disabled Forward 79. In dark, disabled renders at 77 against 201 for enabled. Light users can't tell that history is empty, and the arrows invite clicks that do nothing.
- **Why it hurts the premium feel:** Disabled state is basic platform grammar. AppKit always dims disabled toolbar items.
- **Proposed fix:** Disabled controls use `text_muted.opacity(0.35)` in both appearances; that is 1.68:1 on light chrome, against 6.1:1 for enabled. Add a separate `disabled_glyph()` token so `decorative_muted` keeps its separator meaning.
- **Before:** `mocks/SH/SH-08-before.svg`, `shots/shell/light-nav-disabled-looks-enabled.png` (vs `dark-nav-disabled-reference.png`)
- **After:** `mocks/SH/SH-08-after.svg`
- **Effort:** S

### SH-09 — The sidebar "View options" button keeps a focus ring after mouse use
- **Severity:** P1
- **Category:** states
- **Where:** `crates/ui/src/shell/spaces.rs:1244-1246` (`.in_focus(|el| el.border_color(theme.border_strong))`); `chat-workbench-fixture` `w14-light` (`shots/shell/light-sticky-focus-ring.png`)
- **What's wrong:** The trigger uses `in_focus` instead of `focus_visible`. After you click it, open the menu and close it, focus returns to the button and a 1 px neutral ring stays drawn indefinitely. The ring is `border_strong` (≈ 1.5:1), not the accent focus ring used elsewhere.
- **Why it hurts the premium feel:** A stray outline on a toolbar button after a normal click reads as a rendering glitch.
- **Proposed fix:** `.focus_visible(|s| theme::focus_ring(s, theme.focus_ring()))`. The open state keeps its `glass_hover` fill only while the menu is open.
- **Before:** `mocks/SH/SH-09-before.svg`, `shots/shell/light-sticky-focus-ring.png`
- **After:** `mocks/SH/SH-09-after.svg`
- **Effort:** S

### SH-10 — Chrome controls and icons have no size ladder
- **Severity:** P2
- **Category:** sizing
- **Where:** `crates/ui/src/shell.rs:14486-14529` (window controls 24 / icon 16), `shell.rs:14670-14705` (header buttons 28 / 16), `crates/ui/src/shell/workbench/render.rs:2379-2414` (strip 24 / 13), `render.rs:1330-1360` (tab close 16 / 11), `shell.rs:13655-13680` (right-tab close 18 / 12), `shell.rs:8680-8702` and `:8780-8895` (row actions 20 / 12–13), `crates/ui/src/shell/spaces.rs:1131,1238` and `crates/ui/src/shell/activity_center.rs:704` (29 / 16), `shell.rs:9808-9862` (footer 36 / 19 and 18), `crates/ui/src/shell/usage.rs:3068-3080` (32 / 18), `crates/ui/src/surface_chrome.rs:8-10` (24 / 14)
- **What's wrong:** Chrome controls come in 8 sizes (16, 18, 20, 22, 24, 28, 29, 32/36), including an odd-numbered 29. Icons come in 8 sizes (10, 11, 12, 13, 14, 16, 18, 19). The same `+` glyph is 16 px in the titlebar and 13 px one row lower in the tab strip. The titlebar's own left cluster is 24 px while its trailing buttons are 28 px.
- **Why it hurts the premium feel:** Apple chrome uses a few control sizes (small, regular, large) with matching symbol scales. Ad-hoc sizes make the toolbar rhythm feel hand-placed.
- **Proposed fix:** Add tokens to `surface_chrome.rs`: `CONTROL_S/M/L = 20/28/32`, `ICON_S/M/L = 12/16/18`, radius 5/6/8. S is for inline row actions, tab close and chips. M is for the titlebar, toolbars, tab strip and sidebar header. L is for the sidebar footer. Keep Solar Linear at a single 1.5 stroke.
- **Before:** `mocks/SH/SH-10-before.svg`
- **After:** `mocks/SH/SH-10-after.svg`
- **Effort:** M

### SH-11 — The three sidebar footer buttons disagree on size, tone and hover
- **Severity:** P2
- **Category:** consistency
- **Where:** `crates/ui/src/shell.rs:9800-9875`, `crates/ui/src/shell/usage.rs:3057-3088`; `shots/shell/sidebar-footer-trio.png`
- **What's wrong:** Settings is a 36 px box with a 19 px gear. Devices is 36 px with an 18 px phone. Usage is a 32 px box inside a 36 px wrapper, with an 18 px chart in full `theme.text` (232 against the others' 201, measured). When the usage card is closed, its hover alpha is `0`, so it has no hover state at all. All three hovers snap. The row has an odd `pl(3)`.
- **Why it hurts the premium feel:** Three neighbouring buttons that differ in weight and response feel unfinished. This is the corner your eye returns to every time you open Settings.
- **Proposed fix:** Three L-tier buttons (32 px, 18 px icon, `text_muted`, r8, 4 px gap). Put the first icon's left edge at x = 16, the row-text inset. Hover wash 0.11 with `HOVER_FADE`. An open usage card shows the selected wash (0.16).
- **Before:** `mocks/SH/SH-11-before.svg`, `shots/shell/sidebar-footer-trio.png`
- **After:** `mocks/SH/SH-11-after.svg`
- **Effort:** S

### SH-12 — Hover feedback mixes timing and uses seven different washes
- **Severity:** P2
- **Category:** motion
- **Where:** 48 `.hover(…)` calls against 10 `hover_blend`/`hover_t` uses in `crates/ui/src/shell.rs`, `shell/spaces.rs`, `shell/workbench/render.rs` and others (count by `rg`); `crates/ui/src/motion.rs:405` (`HOVER_FADE` 150 ms)
- **What's wrong:** A sidebar row fades its wash over 150 ms, but its pin, settle and archive buttons snap. Titlebar buttons fade; tab chips, strip buttons, footer buttons and Git footer buttons snap. Hover washes vary: 0.05 (tab), 0.06 (right tab), 0.08 (footer), 0.10 (strip), 0.11 (glass_hover), 0.12 (tab close), 0.16 (chip).
- **Why it hurts the premium feel:** Moving the pointer across the window produces a patchwork of soft and hard responses. Consistent micro-motion is a large part of why macOS feels smooth.
- **Proposed fix:** Add one helper, `chrome_hover(key, rest) -> Hsla`, built on `motion::hover_blend`: hover `wash(0.08)`, pressed `wash(0.12)`, selected `wash(0.16)`, all on `HOVER_FADE`. Replace the shell's `.hover(|s| s.bg(…))` calls with it.
- **Before:** `mocks/SH/SH-12-before.svg`
- **After:** `mocks/SH/SH-12-after.svg`
- **Effort:** M

### SH-13 — Sidebar rows lead with the project, and the trailing corner keeps changing meaning
- **Severity:** P2
- **Category:** typography
- **Where:** `crates/ui/src/shell.rs:8626-9320` (`render_chat_row`: line 1 project at `:9169-9185`, corner at `:8743-8980`, time at `:8944-8951`), `shell.rs:830-832` (`chat_row_height` = 63); `shots/shell/sidebar-selected-row-dark.png`, `related-chats-full.png`
- **What's wrong:** Line 1 is the project label (11 px muted). The title is on line 2, so the eye reads "LUNA" first, and "LUNA" repeats on every row of a single-project list. The trailing corner shows four different things: a time, pin + time, a bare status dot (Done has no word), or three hover buttons. Time and status words are 10 px Medium. Every row is 63 px, so two-line rows float with 15 px of air above and below 33 px of text.
- **Why it hurts the premium feel:** Mail-style lists put the most important line first and keep trailing metadata stable. Here the hierarchy is inverted and the corner flickers between meanings as rows change state.
- **Proposed fix:** Line 1: title 13/500, with time 11 (tabular) at the right. Line 2: meta 11 muted ("LUNA · branch" / kind / status word; drop the project when the filter is one project). Put status and pin in a fixed 16 px glyph slot on line 2. Hover actions replace only the time. Rows are 52 px with an 8 px rhythm, keeping the uniform-height FLIP math (`chat_row_height` returns 52).
- **Before:** `mocks/SH/SH-13-before.svg`
- **After:** `mocks/SH/SH-13-after.svg`
- **Effort:** M

### SH-14 — Active, settled and archived rows use different geometry in one column
- **Severity:** P2
- **Category:** alignment
- **Where:** `crates/ui/src/shell.rs:839-842` (harness 13 + gap 8 vs 14 + gap 10), `shell.rs:9074-9078` (r8), `crates/ui/src/shell/spaces.rs:2195-2201`, `:2480-2500` (36 px, r6, instant hover); `shots/shell/sidebar-hovered-selected-and-archived-rows.png`
- **What's wrong:** Active row titles start at x = 8 + 8 + 13 + 8 = 37. Settled and archived titles start at x = 8 + 8 + 14 + 10 = 40, a 3 px wobble down a single column. Row radii are 8 (active) and 6 (shelves) in the same list. Active rows fade on hover; shelf rows snap.
- **Why it hurts the premium feel:** A text column that shifts by a few pixels between sections is exactly the kind of "almost" alignment that keeps a list from looking crafted.
- **Proposed fix:** Use one leading grid for every row kind: a 16 px icon slot (14 px mark) plus an 8 px gap, so titles sit at x = 40. Radius 8 everywhere. Use the SH-12 hover helper for shelf rows.
- **Before:** `mocks/SH/SH-14-before.svg`
- **After:** `mocks/SH/SH-14-after.svg`
- **Effort:** S

### SH-15 — Sidebar section headers: hairline rules, always-on chevrons, uneven grouping
- **Severity:** P2
- **Category:** consistency
- **Where:** `crates/ui/src/shell/spaces.rs:414-442` (`sidebar_separator`, `sidebar_disclosure_header`), `spaces.rs:383` (28 px), `spaces.rs:2360-2364` (count only when collapsed); `shots/shell/related-chats-full.png`
- **What's wrong:** Every header is a 12 px Medium muted label followed by a hairline that fills the row, with a chevron pinned to the far edge. The chevron points right or down and is always visible; the header has no hover state. Pinned has a header, but the unpinned list below it gets only a bare 1 px separator. "Archived (2)" becomes "Archived" when expanded, so the label changes width on every toggle.
- **Why it hurts the premium feel:** The rule-plus-chevron pattern is Material, not macOS. Uneven grouping makes the list structure harder to scan.
- **Proposed fix:** Use macOS-style headers: 11 px Semibold `text_faint`, no rule, 26 px tall, 12 px space above. Show the chevron and a `wash(0.06)` fill only on hover. Give every group a header ("Pinned" / "Recent"). Always show counts in a quieter weight.
- **Before:** `mocks/SH/SH-15-before.svg`
- **After:** `mocks/SH/SH-15-after.svg`
- **Effort:** S

### SH-16 — Sidebar collapse is a 200 ms clip-wipe
- **Severity:** P2
- **Category:** motion
- **Where:** `crates/ui/src/shell.rs:8375-8396` (fixed-width cached pane inside an `overflow_hidden` width tween), `shell.rs:7701-7715` (`eval_tween`), `crates/ui/src/motion.rs:383` (`RESIZE` = 200 ms, `cubic(0,0,.58,1)`)
- **What's wrong:** On toggle, only the outer width animates. The inner pane stays at its full width, so rows are sliced mid-glyph by a moving clip edge. Nothing translates or fades. The CSS ease-out curve starts linearly and lands abruptly.
- **Why it hurts the premium feel:** macOS sidebars slide out as a unit while the content glides over. A wipe looks like a cropping bug in motion.
- **Proposed fix:** Translate the cached pane by `−(width − now)` so content rides the edge. Fade it over the last 40% of travel. Use 260 ms `cubic-bezier(.32,.72,0,1)` for the sidebar and the right pane. Reduced motion stays a snap.
- **Before:** `mocks/SH/SH-16-before.svg`
- **After:** `mocks/SH/SH-16-after.svg`
- **Effort:** S

### SH-17 — Switching chats blanks the transcript, then pops the new one in
- **Severity:** P2
- **Category:** motion
- **Where:** `crates/ui/src/transcript.rs:3285-3290` ("selection clears the transcript and the replay lands async"), `crates/ui/src/shell.rs:12497-12501` (outlet opacity 0 until `transcript_geometry_ready`, then exactly 1), `crates/ui/src/shell/workbench/render.rs:1510-1519` (tab body swap)
- **What's wrong:** Selecting another chat (sidebar row, ⌘1–9, tab) clears the transcript. The outlet is held at opacity 0 until the bottom stack is re-measured and the replay arrives, then shown at full opacity in a single frame. Workbench tab bodies swap with no transition. The only motion is the 160 ms opacity-in of a new tab chip and new panes fading from 0.4 to 1.
- **Why it hurts the premium feel:** A blank flash followed by a pop is the opposite of the "content is always there" feel of Mail or Messages.
- **Proposed fix:** Keep painting the outgoing transcript until the incoming replay is ready, capped at 250 ms. Then cross-fade: outgoing 1→0 over 100 ms, incoming 0→1 over 160 ms with a 4 px→0 rise, `cubic(.2,0,0,1)`. Use the same recipe for workbench tab-body swaps. Reduced motion: an instant swap with no blank frame.
- **Before:** `mocks/SH/SH-17-before.svg`
- **After:** `mocks/SH/SH-17-after.svg`
- **Effort:** M

### SH-18 — Two different splitters in one window
- **Severity:** P2
- **Category:** interaction
- **Where:** Sidebar and right-pane seam: `crates/ui/src/shell.rs:12237-12333`, `shell.rs:211` (20 px hit, 1 px `border_strong` gradient, faded). Workbench split: `crates/ui/src/shell/workbench/render.rs:602-660`, `render.rs:39-41` (8 px hit, 2 px accent @ 55%, instant)
- **What's wrong:** Resizing the sidebar shows a soft neutral line that fades in. Resizing a pane split two inches away shows a hard 2 px accent bar that snaps on, with a hit target less than half the width.
- **Why it hurts the premium feel:** The same gesture should look and feel the same. NSSplitView never colours the divider on hover.
- **Proposed fix:** Build one `Splitter`: 10 px hit, 1 px hairline 0.08 at rest, raised to 0.14 after a 150 ms hover delay with a 120 ms fade, and 2 px accent only while dragging. Use it for the sidebar, right pane, terminal and workbench splits.
- **Before:** `mocks/SH/SH-18-before.svg`, `shots/shell/workbench-nested-splits.png` (split seams in context)
- **After:** `mocks/SH/SH-18-after.svg`
- **Effort:** S

### SH-19 — Four keyboard-focus ring recipes, two of them nearly invisible
- **Severity:** P2
- **Category:** states
- **Where:** `theme::focus_ring` (2 px inset accent) at `crates/ui/src/shell.rs:8425`, `:12180`. 1 px accent border at `crates/ui/src/shell/workbench/render.rs:1383`. 1 px `border_strong` at `crates/ui/src/shell/tabs.rs:252`, `crates/ui/src/shell/related_chats.rs:183,234`, `crates/ui/src/shell/pane_chat.rs:204`, `crates/ui/src/shell/agents.rs:120`, `crates/ui/src/shell/activity_center.rs:724`, `shell.rs:9022`, `:13226`. `in_focus` at `crates/ui/src/shell/spaces.rs:1246`
- **What's wrong:** The accent rings reach 6.1:1 against the chrome. The neutral `border_strong` rings (white 14% / black 17%) reach about 1.5:1, below the 3:1 non-text minimum the theme itself declares (`NON_TEXT_CONTRAST_MIN`). Keyboard users get a different ring on every control, and some are effectively invisible.
- **Why it hurts the premium feel:** macOS uses one consistent focus ring. A clear, uniform keyboard path is part of feeling native.
- **Proposed fix:** Every focusable shell control uses `.focus_visible(|s| theme::focus_ring(s, theme.focus_ring()))`, which has zero layout cost and follows the control's radius. Remove the `border_1().border_color(transparent)` scaffolding these controls carry only for the ring.
- **Before:** `mocks/SH/SH-19-before.svg`
- **After:** `mocks/SH/SH-19-after.svg`
- **Effort:** S

### SH-20 — Chrome text drops to 9–10 px, across 15 sizes in the shell
- **Severity:** P2
- **Category:** typography
- **Where:** for example `crates/ui/src/shell/command_palette.rs:848` (section header 9.5), `:732` (9.5), `crates/ui/src/shell.rs:8947` (row time 10), `shell.rs:8933` (status word 10), `crates/ui/src/shell/workbench/render.rs:1313` (Main badge 10), `crates/ui/src/popover.rs:1028` (kbd 10), `crates/ui/src/shell/spaces.rs:1196` (device tag 10), `command_palette.rs:1179` (footer hints 10)
- **What's wrong:** Across `shell/*`, `rail.rs`, `history.rs`, `surface_chrome.rs`, `notice.rs` and `badges.rs`, there are 15 distinct `text_size` values: 9, 9.5, 10, 10.5, 11, 11.5, 12, 12.5, 13, 14, 15, 18, 20, 29 and 36. 46 calls are below 11 px. Half-pixel steps (11 vs 11.5, 12 vs 12.5) are visually indistinguishable but break rhythm.
- **Why it hurts the premium feel:** Apple's chrome floor is 11 pt, and its hierarchy uses a handful of named styles. Tiny labels strain the eye, and too many near-identical sizes blur the hierarchy.
- **Proposed fix:** Add `typography::TextRole { Title 15/20/600, Headline 13/17/600, Body 13/17/400, Callout 12/16/400, Caption 11/14/500 }` resolving to `ui_rems` size, line height and weight. Map all 9–10.5 literals to Caption, and replace the 11.5 and 12.5 values with 11/12/13.
- **Before:** `mocks/SH/SH-20-before.svg`
- **After:** `mocks/SH/SH-20-after.svg`
- **Effort:** M

### SH-21 — Two tooltip styles and three delays
- **Severity:** P2
- **Category:** consistency
- **Where:** `crates/ui/src/shell.rs:1243-1258` (`SurfaceTabTooltip`: r6, pad 9/6, 10.5 px muted, border, no shadow), `crates/ui/src/pickers.rs:571-610` (`ControlTooltip`: r8, pad 10/7, 12 + 11 px, `shadow_md`), delays 300 ms ×3, 350 ms ×13, 400 ms ×9 (`pickers.rs:520`)
- **What's wrong:** A row's pin tooltip and the tab strip's `+` tooltip, a few pixels apart, use different cards, type sizes and delays. Every move between controls waits the full delay again. Sidebar row detail cards open after 300 ms, so they pop up while you scan the list.
- **Why it hurts the premium feel:** macOS tooltips are one style with "warm" behaviour: after the first one appears, siblings show immediately.
- **Proposed fix:** Use one tooltip: r8, pad 10/6, title 12 with optional 11 muted detail, shortcut right-aligned, `shadow_md`, frost. Delay 500 ms for the first, 0 ms for a sibling within 1 s, fade in 100 ms. Row detail cards: 700 ms.
- **Before:** `mocks/SH/SH-21-before.svg`
- **After:** `mocks/SH/SH-21-after.svg`
- **Effort:** S

### SH-22 — Tab activation reflows the strip, and the close glyph snaps
- **Severity:** P2
- **Category:** motion
- **Where:** `crates/ui/src/shell/workbench/render.rs:1386-1391` (active → `FontWeight::MEDIUM`), `render.rs:1340-1341` (close `opacity(if active {1} else {0})` + `group_hover`), `render.rs:1390` (inactive hover `wash(0.05)`, instant)
- **What's wrong:** Activating a tab switches its title from Regular to Medium. The wider text pushes every following tab about 5 px right, so the strip shifts on click. The close glyph flips 0→1 on hover with no fade, and the inactive hover wash is instant.
- **Why it hurts the premium feel:** Safari tabs never move when you select one. Layout shifting on selection makes the strip feel unstable.
- **Proposed fix:** Use one weight for every tab (Medium). The active tab reads through its fill and full text colour (`tab_title`). Reserve the 16 px close slot; fade the glyph to 0.7 on hover and 1.0 when active over 120 ms, using the SH-12 helper.
- **Before:** `mocks/SH/SH-22-before.svg`
- **After:** `mocks/SH/SH-22-after.svg`
- **Effort:** S

### SH-23 — The right-pane picker looks like a wireframe, on a different surface from the centre
- **Severity:** P2
- **Category:** consistency
- **Where:** `crates/ui/src/shell.rs:13201-13358` (`render_surface_picker`: 44 px outlined rows, r10, `max_w(280)`, vertically centred), `shell.rs:13045-13200` (right pane flush on chrome), `crates/ui/src/shell/workbench/render.rs:142-183` (centre `contained_plane`); `shots/shell/right-pane-surface-picker.png`
- **What's wrong:** An empty right pane shows eight outlined 44 px buttons stacked in the middle of blank space, with no heading. The centre column is a rounded, hairlined canvas, while the right pane's content sits flush on the frost, so the two halves of the window use different surface languages.
- **Why it hurts the premium feel:** Outlined button stacks read as placeholder UI, and mismatched surfaces make the layout look unfinished.
- **Proposed fix:** Render the right pane in the same `contained_plane` (r14, hairline, canvas fill). For the picker, add a 13/600 heading ("Open in this pane") with an 11 px hint, then a 2-column grid of 52 px tiles: 20 px icon, 12.5 px label, `wash(0.05)` fill, no outline, 8 px gap, with counts in an accent capsule.
- **Before:** `mocks/SH/SH-23-before.svg`, `shots/shell/right-pane-surface-picker.png`
- **After:** `mocks/SH/SH-23-after.svg`
- **Effort:** M

### SH-24 — Three empty states, three different visual languages
- **Severity:** P2
- **Category:** states
- **Where:** `crates/ui/src/shell.rs:12511-12560` (empty project: ASCII moon + 20 px Semibold + solid button), new-thread canvas (`render_canvas_frame`, bare composer; title intentionally blank at `crates/ui/src/shell/tabs.rs:273-279`), `shell.rs:9701-9718` (sidebar "No sessions yet", 12 px faint, top-left); `shots/shell/empty-project-dark.png`, `new-thread-canvas-dark.png`
- **What's wrong:** The empty project screen is illustrated with a 20 px headline and a primary button. The new-thread canvas is a lone composer with no title or context. The empty sidebar is a single 12 px faint line pinned to the top-left with no action, which duplicates the canvas message.
- **Why it hurts the premium feel:** Empty states are first impressions. Three unrelated treatments make onboarding feel stitched together.
- **Proposed fix:** Build one `EmptyState` (illustration or icon, 15/600 title, 12 secondary line, optional L-tier action, centred, `fade_in`) and keep the ASCII moon for the project case. For the new-thread canvas, add a quiet 15/600 "New thread" title with a "project · device" line above the composer. That respects the request for no titlebar title: this lives in the canvas. For the sidebar: "No tasks yet" with a "Start one with ⌘N" button.
- **Before:** `mocks/SH/SH-24-before.svg`, `shots/shell/empty-project-dark.png`, `shots/shell/new-thread-canvas-dark.png`
- **After:** `mocks/SH/SH-24-after.svg`
- **Effort:** M

### SH-25 — Palette rows: three heights, tiny headers, non-concentric radius
- **Severity:** P3
- **Category:** spacing
- **Where:** `crates/ui/src/shell/command_palette.rs:891-895` (actions `min_h 30`), `:701-705` (history `min_h 42`), `:916-985` (reuses the 63 px sidebar row), `:838-851` (separator + `my(7)` + 9.5 px header), `:1081` (card r16) with `:1004` (8 px inset), `crates/ui/src/popover.rs:310` (`PALETTE_ITEM_RADIUS = 14 − 4 = 10`)
- **What's wrong:** One list mixes 30 px action rows, 63 px three-line chat rows and 42 px history rows. Groups are separated twice, by a rule with 7 px margins and by a 9.5 px header. Rows use r10 inside a r16 card at an 8 px inset; the concentric radius would be 8.
- **Why it hurts the premium feel:** A palette lives on rhythm. Uneven row heights and non-concentric corners make it look assembled rather than designed.
- **Proposed fix:** Use two row heights: 32 (single-line action, shortcut right-aligned) and 44 (thread: title 13/500 plus an 11 px meta line). Headers 11 px Semibold, separated by 10 px of space only. Set `PALETTE_ITEM_RADIUS = 16 − 8 = 8`. Footer hints at 11 px.
- **Before:** `mocks/SH/SH-25-before.svg`
- **After:** `mocks/SH/SH-25-after.svg`
- **Effort:** S

### SH-26 — The Activity bell's unread dot floats off the glyph
- **Severity:** P3
- **Category:** alignment
- **Where:** `crates/ui/src/shell/activity_center.rs:704-760` (29 px box, 5 px dot at `top 2 / right 2`)
- **What's wrong:** The dot is placed on the 29 px button's corner, not on the 16 px glyph. It sits about 4.5 px clear of the bell and reads as a stray pixel (visible in `shots/shell/tab-strip-main-scrolled-away.png`). Other unread dots in the shell are 6 px.
- **Why it hurts the premium feel:** iOS and macOS badges hug the symbol's shoulder with a knockout ring. A detached dot looks misplaced.
- **Proposed fix:** Add a shared `UNREAD_DOT = 6` token. Centre the dot on the glyph's top-right shoulder, about (19.5, 8.5) in a 28 px M-tier box, with a 1.5 px chrome-coloured knockout ring.
- **Before:** `mocks/SH/SH-26-before.svg`
- **After:** `mocks/SH/SH-26-after.svg`
- **Effort:** S

---

## Top 5 fixes with the biggest premium payoff

1. **Make selection unmistakable (SH-02).** This means selected 0.16 with a hairline, hover 0.06 and a fade. It is the single most-seen state in the app and only a few lines of tokens to change.
2. **Fix the tab strip (SH-03 + SH-06 + SH-22).** Use one `TabChip`, pin Main, add edge fades and an overflow menu, and stop the weight reflow. Tabs are the workbench's spine, and right now they clip, shift and come in two dialects.
3. **Calm the command palette (SH-04 + SH-25).** Pin the search field at 20% from the top, animate it in with a scale fade over 160 ms, and use two row heights. This turns a web-modal feel into Spotlight.
4. **Make light mode white paper with real disabled states (SH-07 + SH-08).** These are two token changes that make the whole light theme read as a proper macOS app.
5. **Unify motion and feedback in the chrome (SH-12 + SH-16 + SH-17 + SH-18).** That means one hover helper on `HOVER_FADE`, a sliding sidebar, cross-faded chat switches and one splitter. This is what makes everything feel smooth, the user's core ask.
