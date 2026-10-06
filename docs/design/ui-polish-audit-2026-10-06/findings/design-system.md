# Design-system foundations (DS)

Audit of the system underneath every LUNA surface: theme tokens, typography,
spacing and radius, icons, motion and shared controls, with Settings as the
largest consumer. Source baseline `590cf4b6`; all counts come from `rg` over
`crates/ui/src` (164 files, ~204k lines) and from headless renders of the
production views. No product code was changed.

## Area overview

LUNA's **colour layer is in good shape**. The 2026-09-29 appearance audit has
held: `appearance-audit` measures 810 theme × surface × accent combinations
with **0 failing contrast pairs**, and production code contains only 12 raw
`rgb()`/`hsla()` literals. All of them are intentional: provider brand colours,
the Windows close-button red, image-overlay scrims and a test fixture.

Everything else is **local convention rather than system**. Values are typed
in place at each call site, and almost none of them come from tokens:

- **Type.** 670 `text_size` calls use 17 distinct sizes. Only 112 `line_height`
  calls pair with them, so most text inherits GPUI's 1.618 leading.
- **Spacing and radius.** 1,432 literal padding, gap and margin values use 32,
  20 and 28 distinct numbers; `Theme::SPACE_*` covers about 8% of spacing.
  296 literal radii use 15 numbers (17 with named constants).
- **Motion.** A good catalog exists in `motion.rs`, but there are 16 distinct
  UI durations and 6 curves, and no spring. Most hover and every
  switch/segment selection still snaps. macOS **Reduce motion is never read**.
- **Controls.** No shared `Button`, `Select`, `Segmented` or `IconButton`
  exists. Nine button/picker recipes ship. The Appearance page alone shows
  five control heights and two chevron glyphs for the same dropdown role.
- **Icons.** Only about half the icon set (~57 of 112) was fetched from Solar.
  About 47 glyphs are hand-drawn or ported, with three different canvases and
  four stroke weights.
- **Settings.** Rows use three different anatomies, each 35–60% taller than
  macOS System Settings. Group headers float halfway between groups. At the
  supported 20 px interface size, a row description runs underneath its
  control.

The fixes are mostly mechanical once the tokens exist. That is why this file
ends with a concrete token set and a ranked list of the five highest-payoff
changes.

### Fixtures run (all headless, outputs in the session scratchpad `ds/`)

| Fixture | Output used |
| --- | --- |
| `appearance-fixture` | Appearance page light/dark, 12/16/20 px, frosted/opaque (`appearance-*.png`) |
| `appearance-audit` | `contrast.md` / `contrast.json`: 810 combos, 0 failing |
| `usage-fixture … 280 local-settings` | `settings-local.png` (Settings › General at 600 pt) |
| `mcp-inventory-fixture` | Settings › Providers cards (`01-claude-mcp-inventory.png`) |
| `claude-extensions-fixture` | Providers › Claude extensions (`02/03-extensions-*.png`) |

Not run: `accounts-fixture` opens a real, visible window. It is not headless,
and the repo allows only one GUI app at a time. `app-icon-fixture` was out of
scope for visual tokens.

Evidence crops are in `shots/design-system/`.

## Measured inventory

Raw tallies are in the session scratchpad
(`/private/tmp/claude-501/…/scratchpad/ds/*.txt`, `icon_inventory.tsv`).

### Font sizes: `text_size(ui_rems(N))`, pt at the 16 px default

| Size | 9 | 9.5 | 10 | 10.5 | 11 | 11.5 | 12 | 12.5 | 13 | 13.5 | 14 | 15 | 16 | 18 | 20 | 29 | 36 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Sites | 3 | 3 | 57 | 31 | 223 | 53 | 157 | 18 | 56 | 1 | 20 | 4 | 1 | 3 | 2 | 1 | 1 |

- **17 distinct sizes**, 5 of them half-points (106 sites).
- 14 more named size constants (`TOOL_TEXT_SIZE` 12, `INPUT_TEXT_SIZE` 14,
  `MD_TEXT_SIZE` 14, `QUEUE_TEXT_SIZE` 12.5 …) re-declare values from the same
  set.
- **670** `text_size` calls vs **112** `line_height` calls. Text without a
  paired line height inherits `line_height: phi()` = 1.618
  (`vendor/zui/crates/gpui/src/style.rs:490`).
- Weights: `MEDIUM` ×97, `SEMIBOLD` ×26, `NORMAL` ×13, `BOLD` ×5.
- Top files: `shell.rs` 67, `pickers.rs` 51, `shell/usage.rs` 47,
  `composer.rs` 33, `settings/appearance.rs` 27.

### Spacing: literal `px(N)` in padding / gap / margin calls

| Kind | Sites | Distinct values | On 4-pt grid | Even, off-grid | Odd / fractional |
| --- | --- | --- | --- | --- | --- |
| padding (`p, px, py, pt, pb, pl, pr`) | 625 | 32 | 53% | 35% | 12% (3, 5, 7, 9, 11, 13, 15, 21) |
| gap (`gap, gap_x, gap_y`) | 499 | 20 | 46% | 40% | 13% |
| margin (`m*`) | 308 | 28 | 65% | 30% | 5% |

- `Theme::SPACE_XS/SM/MD/LG` are referenced **125×** against **1,432**
  literals.
- Most-used values: padding 8 ×125, 6 ×82, 10 ×79; gap 8 ×130, 6 ×96, 2 ×68.
- Top files: `shell.rs` 159, `settings/appearance.rs` 123, `pickers.rs` 93,
  `shell/usage.rs` 81.

### Corner radii

| Radius | 1 | 2 | 3 | 3.5 | 4 | 4.5 | 5 | 5.5 | 6 | 7 | 8 | 9 | 10 | 12 | 14 | 16 | 26 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Uses (literal + named) | 3 | 7 | 10 | 1 | 38 | 1 | 25 | 1 | 115 | 25 | 62 | 14 | 17 | 16 | 6 | 5 | 1 |

- **296** literal `rounded(px(N))` calls plus 19 `*_RADIUS` constants. Two
  name collisions: `CARD_RADIUS` is 12 in `popover.rs:302` and 16 in
  `composer/goals.rs:77`; `PANEL_RADIUS` is 10 in `theme.rs:1304` and 16 in
  `queue.rs:101`.
- `Theme::PANEL_RADIUS` is used once. `rounded_full` ×57.

### Motion

| Duration (ms) | Where |
| --- | --- |
| 100 | `MENU_OUT` |
| 120 | `PREVIEW_FADE_MS` (workbench) |
| 140 | `MENU_IN`, `TOOL_FOLD` (transcript.rs:120) |
| 150 | `FADE_QUICK`, `TAB_SLIDE`, `HOVER_FADE`, `TAB_EXIT_MS` |
| 160 | `INSERT_MS` (workbench) |
| 170 | `COLLAPSE_MS` (shell/workbench.rs:257) |
| 180 | `DIALOG_IN`, `COLLAPSE` |
| 200 | `RESIZE`, `CHEVRON` |
| 220 | `RESIZE_EDGE_BOUNCE_MS`, `AUTO_ADVANCE_MS` |
| 240 | composer morph (composer.rs:8547) |
| 250 | `ROUTE_SNAP_MS` |
| 260 | `RESORT` (shell.rs:785), `MENU_SCROLLBAR_FADE_MS` |
| 360 / 420 / 480 / 500 | `TOOL_ROW_REVEAL`, `NEW_THREAD_TRANSITION`, transcript settle, `FADE_IN` / `SPLASH_OUT` / `SCROLL_GLIDE` |
| loops | `GRADIENT_SPIN` 750, `LUNA_PULSE` 2400, row stagger 65/90 |

- Curves: `EASE_OUT_EXPO`, `EASE_OUT`, `EASE`, `EASE_OUT_QUINT`
  (= `EASE_RESORT`), `EASE_IN_OUT`, `EASE_TAILWIND`. That is 6 curves, with
  no spring for any disclosure.
- 9 `MotionSpec::new` calls sit outside `motion.rs`. 22 `with_animation`
  sites. Hover: 175 `.hover()` closures snap instantly, while 38
  `motion::hover_blend` sites fade over 150 ms.
- Tooltip delays: 250, 280, 300 (×4), 350 (×25), 400 (×13) ms, plus the GPUI
  default of 500 ms.

### Colour literals and alpha ladders

| Kind | Sites | Distinct values | Notes |
| --- | --- | --- | --- |
| `rgb(0x…)` / `hsla(…)` outside `theme.rs` | 12 | – | brand (Claude, OpenCode), Windows close red, image overlay, loaders, 1 test |
| `gpui::white()/black()/transparent_*()` | 47 | – | mostly transparent borders and overlays |
| `ink(α)` | 150 | 24 alphas (0.015–0.7) | untinted white (dark) / black (light) |
| `wash(α)` | 61 | 14 alphas | untinted |
| `hairline(α)` | 45 | 12 alphas | untinted |
| `.opacity(literal)` | 435 | 50 values | on colours and elements |
| `readable_muted(α)` | 101 | 10 alphas (0.45–0.9) | secondary text |
| Hover fills `.hover(\|s\| s.bg(…))` | 116 (single-line matches) | 40 expressions, ≈25 colour recipes | 66 use raw `ink`/`wash` |

### Icons (`crates/ui/assets/icons`, 112 files)

| Group | Count | Canvas / stroke | Examples |
| --- | --- | --- | --- |
| Fetched Solar Linear | ≈57 | 24 / 1.5, round caps | folder, copy, magnifer, eye, refresh |
| Hand-drawn "in the Solar style" (per `icons.rs` comments) | ≈40 | 24 / 1.5 (mostly) | floppy-disk, git-branch, return, info-circle, bot, bell, star, file-* |
| Ported luna glyphs, 16 px canvas | 7 | 16 / 1.25–1.6 | close, plus, terminal, window-min/max/restore, check |
| Off-weight 24 px | 3 | 1.6 or 2.0 | chart 1.6, pin 1.6, pause 2.0 |
| Mirrored / rotated copies | 4 | – | arrow-down, arrow-up-right, alt-arrow-up, sidebar-left |
| Outline-expanded fills | 2 | fill paths | moon, key-minimalistic |
| Brand marks | 4 | own viewBox | claude, openai, opencode, luna-logo |

- Render sizes: **17 distinct icon sizes**: 9, 10, 10.5, 11, 12 (×43),
  12.5, 13 (×35), 14 (×44), 15, 16 (×52), 17, 18, 19, 20, 22, 28.
- The same glyph appears at up to 5 sizes: `PLUS` 11/12/13/14/16, `CHECK`
  10/12/13/14/16.

### Shared controls

| Control | Implementations found |
| --- | --- |
| Button | `popover::button_frame` (+ghost/primary/danger), `widgets::ghost_action`, `appearance::compact_action`, `surface_chrome` control, `harnesses/mcp::mcp_button` (`gpui_base::Button`), queue action, `changes::header_button`, `markdown` code action. 9 recipes; heights 24 / 28 / 30 / ≈31 / 32 / ≈33 / 34 / 36 |
| Select / dropdown trigger | theme selector (h34 r8 12.5 pt, `SORT_VERTICAL`), font/size picker (h36 r9, inherits 1 rem, `ALT_ARROW_DOWN`) |
| Segmented | Appearance separate pills (h30 r7), General joined track (r9/r7), background-effect chips (h28 r7 11 pt) |
| Switch | one `toggle_switch` (good), 32×18 with a 14 pt knob, no animation |
| Focus | 67 `focus_visible` sites using 7 treatments; `AccessibleButton` helper is opt-in (46 uses) |
| Scrollbars | popover menu rail (shared), terminal, shortcuts page and `gpui_base` themed, at 3 → 5 px |

## Findings

### DS-01 — No type scale; 17 sizes and borrowed 1.618 leading
- **Severity:** P1
- **Category:** typography
- **Where:** `crates/ui/src/typography.rs:121` (`ui_rems` accepts any number);
  root `crates/ui/src/shell.rs:14900` sets 14 pt with no line height; default
  leading from `vendor/zui/crates/gpui/src/style.rs:490`; Settings row metrics
  in `crates/ui/src/settings/widgets.rs:151-152`.
- **What's wrong:**
  - There is no type ramp. Call sites type a size: 17 distinct literal sizes
    across 634 `text_size(ui_rems(N))` calls, 5 of them half-points (9.5,
    10.5, 11.5, 12.5, 13.5; 106 sites). 11, 11.5, 12, 12.5 and 13 are used
    interchangeably for the same roles. Row descriptions alone use 11.5
    (`general.rs:264`), 12 (`widgets.rs:152`) and 12 with an 18 pt line
    (`appearance.rs:3728`).
  - Only 112 of 670 size declarations pair a line height. The rest inherit
    φ = 1.618, so a 12 pt description gets a 19.4 pt line instead of 16.
    That alone makes every two-line Settings row about 6 pt taller (DS-08).
  - Weight carries hierarchy almost alone: `MEDIUM` is used ×97, `SEMIBOLD`
    ×26.
- **Why it hurts the premium feel:** Apple UIs feel calm because a few text
  styles repeat with exact leading. Here, adjacent labels differ by half a
  point and a different line box, which reads as subtle misalignment
  everywhere.
- **Proposed fix:** Add `typography::TextStyle` (caption2 10/13, caption
  11/14, footnote 12/16, body 13/18, prose 14/21, title3 15/20, title2 17/22,
  title1 20/25, display 26/32). Add `Styled::text_style(TextStyle)`, which
  sets size, line height and weight together, all scaling through
  `ui_rems`. Set `body` on the shell root. Then codemod the 634 sites using
  the "absorbs" mapping in the after-mock. Keep code and terminal sizes
  separate, as they are today.
- **Before:** `mocks/DS/DS-01-before.svg`
- **After:** `mocks/DS/DS-01-after.svg`
- **Effort:** L

### DS-02 — Nine button/picker recipes; five heights on one page
- **Severity:** P1
- **Category:** consistency · sizing
- **Where:**
  - `crates/ui/src/settings/appearance.rs:2314`: theme selector, h34 r8,
    12.5 pt, `SORT_VERTICAL`.
  - `appearance.rs:2106` / `:2218`: font/size pickers, h36 r9, label without
    `text_size`, so it inherits 1 rem = 16 pt; `ALT_ARROW_DOWN`.
  - `:1374`: surface chip, h30 r7, 11.5 pt.
  - `:1673` `compact_action`: h28 r7, 11.5 pt.
  - `crates/ui/src/popover.rs:1140-1191` `button_frame` / `btn_primary`:
    py5 + 13 pt × φ ≈ 33 pt, r8.
  - `crates/ui/src/settings/widgets.rs:488` `ghost_action`: no rest chrome,
    ≈31 pt.
  - `crates/ui/src/settings/harnesses/mcp.rs:48` `mcp_button`: h24 r6.
  - `crates/ui/src/queue.rs:1278`: h28 r5.
  - `crates/ui/src/surface_chrome.rs:8`: 24 / r6.
  - Shots: `shots/design-system/appearance-light-picker-recipes.png`,
    `providers-prose-card-chromeless-buttons.png`.
- **What's wrong:**
  - The right-hand control column on Appearance stacks controls at 28, 30,
    33, 34 and 36 pt, with radii 7, 8 and 9.
  - Labels are 11.5, 12.5 or about 16 pt. The measured cap height of "Geist"
    is 11.5 pt against 9 pt for "Luna Light" next to it.
  - Two chevrons (⇅ and ⌄) serve the same dropdown role.
  - On Providers, `ghost_action` buttons ("Refresh models", "Check
    provider", "New global skill") have no fill or border at rest, so they
    read as body text.
- **Why it hurts the premium feel:** macOS controls come in a few sizes, and
  every push button and pop-up on a pane shares one height and corner. Mixed
  heights in one column are the most visible "assembled from parts" signal.
- **Proposed fix:**
  - Build one `ui::Button` / `ui::IconButton` / `ui::Select` set with three
    sizes: `sm` 24 (r6, 11 pt, px 8), `md` 28 (r8, 12 pt, px 10) and `lg` 32
    (r8, 13 pt, px 12).
  - Use four variants: primary (solid), secondary (fill + separator), ghost
    (6% rest fill) and danger.
  - Give `Select` one chevron: `alt-arrow-down` at 14 px.
  - Use `md` throughout the Settings control column, `lg` in dialogs and
    `sm` in toolbars.
  - Delete the recipes listed above.
- **Before:** `mocks/DS/DS-02-before.svg` (+ `shots/design-system/appearance-light-picker-recipes.png`)
- **After:** `mocks/DS/DS-02-after.svg`
- **Effort:** M

### DS-03 — Settings row text runs under its control; no reflow
- **Severity:** P1
- **Category:** alignment · states
- **Where:** `crates/ui/src/settings/appearance.rs:3464-3488` (Accent row);
  `crates/ui/src/settings/widgets.rs:350` `meta_line`, whose fragments are
  non-shrinking flex items; `crates/ui/src/settings/general.rs:252-320`.
  Shots: `shots/design-system/appearance-20px-accent-description-overlap.png`,
  `settings-general-narrow-row-collapse.png`.
- **What's wrong:**
  - At the supported 20 px interface size, the Accent row description
    ("…code, and activity.") paints about 30 pt underneath the first accent
    swatch. The label column is `flex_1().min_w_0()`, but the `meta_line`
    fragment does not wrap or shrink, so its text overflows the column.
  - The same structure fails the other way when content is narrow. In
    General at 600 pt, the title truncates to "Se…" and the description
    wraps one word fragment per line ("Choo / se / whet / her"). The
    segmented control is `flex_none` and never drops below the label.
- **Why it hurts the premium feel:** Text colliding with a control is the
  kind of defect people screenshot. System Settings never lets a label and
  a control overlap; it wraps the label or stacks.
- **Proposed fix:**
  - Make `settings::row` a real two-column grid. The label column gets
    `flex_1`, `min_w_0` and min 200 pt, with wrapping text and no fragment
    `flex_none`. The control column gets `flex_shrink_0` and `max_w(50%)`,
    with a 16 pt gap.
  - When the content width is below 520 pt, switch to `flex_col` with the
    control under the description, 8 pt apart.
  - Make accent swatches 22 pt dots so the control column is 30% narrower.
- **Before:** `mocks/DS/DS-03-before.svg` (+ both shots)
- **After:** `mocks/DS/DS-03-after.svg`
- **Effort:** S

### DS-04 — macOS "Reduce motion" is never read
- **Severity:** P1
- **Category:** motion
- **Where:** `crates/ui/src/motion.rs:836` (`set_reduced_motion`): the only
  callers are `examples/chat-workbench-fixture.rs:5459-5498` and
  `examples/empty-project-fixture.rs:236`. Nothing in `apps/luna`,
  `crates/ui/src/lib.rs` or `appearance.rs` observes
  `NSWorkspace.accessibilityDisplayShouldReduceMotion`.
- **What's wrong:**
  - The reduced-motion plumbing is complete. GPUI snaps every
    `with_animation`, and 13 files branch on `reduced_motion`. But
    `App::reduce_motion` is always `false` in the shipped app.
  - With Reduce motion on, panes still tween 200 ms, new-thread still glides
    420 ms, and loaders loop.
  - Reduce transparency and Increase contrast are not observed either; the
    frost and contrast systems would map onto them naturally.
- **Why it hurts the premium feel:** Honouring system accessibility
  settings is table stakes for a native Mac app. Users who set Reduce motion
  experience every animation as a bug.
- **Proposed fix:** Add `appearance::observe_accessibility(cx)`. At launch
  and on `NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification`, call
  `motion::set_reduced_motion(cx, value)`. Map Reduce transparency to
  `SurfacePreference::Opaque`, and Increase contrast to `border_strong`
  borders and a higher readable floor. Under reduce motion the motion tokens
  (DS-15) return a 120 ms opacity-only crossfade.
- **Before:** `mocks/DS/DS-04-before.svg`
- **After:** `mocks/DS/DS-04-after.svg`
- **Effort:** S

### DS-05 — 25 hover fills and two hover behaviours
- **Severity:** P1
- **Category:** interaction · motion
- **Where:** 175 `.hover()` closures across `crates/ui/src`. Fills include
  `ink(0.015…0.09)`, `wash(0.05…0.14)`, `glass_hover()`,
  `surface_raised_hover`, `element_hover`, `accent.opacity(0.12)` and
  `tone.opacity(0.16)` (tally: `scratchpad/ds/hover_bg_values.txt`).
  Settings rows: `crates/ui/src/settings/widgets.rs:309` (`ink(0.015)`). The
  animated path is `motion::hover_blend` (38 sites, `motion.rs:810`).
- **What's wrong:**
  - Hover backgrounds use about 25 distinct colour recipes for the same
    role. Ten different `ink` alphas from 1.5% to 9% sit side by side.
  - About 80% of hovers snap in 0 ms, while menus and dialog buttons fade
    over 150 ms, so the same pointer gesture feels different per surface.
  - Every Settings card row hovers at 1.5% white (#060606 → #0a0a0a). That
    is below perception, so rows feel dead.
- **Why it hurts the premium feel:** Hover is the most frequent micro-
  interaction. One consistent, softly animated hover is a large part of what
  makes macOS lists feel "liquid".
- **Proposed fix:**
  - Add three interaction fills, derived from `theme.text` (DS-14):
    `fill.tertiary` 6% (hover), `fill.secondary` 10% (pressed) and
    `accent.wash` (selected).
  - Apply them only through `ui::interactive(id)`, which wraps
    `motion::HoverFades`: 120 ms in, 180 ms out, standard curve.
  - Settings rows use the same 6%.
  - Lint new `.hover(|s| s.bg(ink(…)))` calls.
- **Before:** `mocks/DS/DS-05-before.svg`
- **After:** `mocks/DS/DS-05-after.svg`
- **Effort:** M

### DS-06 — Switches, segments and swatches snap; the switch is off-brand
- **Severity:** P1
- **Category:** motion · states
- **Where:** `crates/ui/src/settings/widgets.rs:417-450` `toggle_switch`
  (knob `left(if on {16} else {2})`, track `theme.text` when on, 32×18,
  14 pt knob; 15 call sites). Selection chips at `appearance.rs:1374`,
  `:1422`, `:1500` and `:1858`, and `general.rs:270-318`.
- **What's wrong:**
  - Clicking a switch moves the knob 14 pt in one frame and swaps the track
    from #2a2a2a to near-white (#e5e5e5). The on state is not the accent;
    the measured knob is #0e0e0e on a white track.
  - Segmented options, accent swatches, option cards and Glass chips swap
    border, fill and text colour instantly.
  - None of the 15 switches animate.
- **Why it hurts the premium feel:** The iOS/macOS switch's travelling knob
  and colour crossfade is the most recognisable "Apple" micro-motion.
  Teleporting thumbs feel like a web prototype.
- **Proposed fix:**
  - Size the switch 38×22 (macOS regular) or 26×15 (small) with an 18 pt
    knob. Use the accent track when on.
  - Move the knob on a `motion::DISCLOSURE`-class spring (response 0.32 s,
    damping 0.86) and crossfade the track over 160 ms.
  - Give segmented controls a single sliding thumb (`motion::QUICK`), and
    let selection rings scale in from 0.92.
  - Use cross-fade only under reduce motion.
- **Before:** `mocks/DS/DS-06-before.svg`
- **After:** `mocks/DS/DS-06-after.svg`
- **Effort:** M

### DS-07 — Settings group headers float between groups
- **Severity:** P1
- **Category:** spacing
- **Where:** `crates/ui/src/settings/widgets.rs:203` (`field_label`) +
  `:290-293` (`section_card` adds `mt(24)`) inside the callers'
  `.flex_col().gap(12)`, e.g. `crates/ui/src/settings/appearance.rs:3905-3912`.
  Shot: `shots/design-system/appearance-light-group-header-spacing.png`.
- **What's wrong:** Measured on the Appearance render, "Code & diffs" sits
  **46.5 pt** below the previous row's text and **42.5 pt** above its own
  card. The 12 pt gap plus the card's own 24 pt top margin put the label
  equidistant between groups, so it does not visually belong to either. The
  same stack is used for "Reset" and by the other pages that use
  `field_label` + `section_card`.
- **Why it hurts the premium feel:** The proximity principle carries the
  structure in System Settings: headers hug their group. Floating headers
  make the page look like a loose list of parts.
- **Proposed fix:** Add `settings::group(title, rows)`, which owns the
  spacing: 24 pt before a group header (`space.6`) and 8 pt from header to
  card (`space.2`). Remove `mt(24)` from `section_card`. The header style is
  covered in DS-22.
- **Before:** `mocks/DS/DS-07-before.svg` (+ shot)
- **After:** `mocks/DS/DS-07-after.svg`
- **Effort:** S

### DS-08 — Three settings-row anatomies, all oversized
- **Severity:** P1
- **Category:** consistency · sizing
- **Where:**
  - `crates/ui/src/settings/widgets.rs:304-345` (`card_row` px20 py14 gap14,
    `row_tile` 36 r10, `row_title` 13 M, `meta_line` 12): **70.5 pt rows**,
    measured.
  - `crates/ui/src/settings/general.rs:252-330` (`min_h(84)`, description
    11.5 with mt 4, no tile).
  - `crates/ui/src/settings/harnesses.rs` (Providers cards: title + paragraphs
    + chrome-less `skill_button` actions, `harnesses.rs:49`).
  - Font rows outside any card: `appearance.rs:3715-3735`.
  - Shots: `appearance-light-overview` (scratchpad),
    `shots/design-system/providers-prose-card-chromeless-buttons.png`.
- **What's wrong:**
  - The same "setting + control" unit is drawn three ways, at 70.5 and
    84 pt tall.
  - System Settings uses about 36–40 pt single-line rows and about 50–56 pt
    rows with a subtitle, with a 20 pt icon tile.
  - Providers renders diagnostics as paragraphs inside cards ("Claude
    reports it connected for this project (dynamic scope)…") with
    full-width text actions, so it reads like a log rather than settings.
  - The Appearance font rows sit outside any card, with no tile, beside
    carded rows.
- **Why it hurts the premium feel:** Consistent, compact rows are the
  backbone of a polished preferences window. Tall rows plus prose make
  Settings feel like a web admin page.
- **Proposed fix:**
  - Build one `settings::row(icon, title, description, control)`: 52 pt
    with a description, 40 pt single-line, px 16, gap 12.
  - Tile 28×28 at r7 with a 16 px icon. Title body 13/18 Medium,
    description footnote 12/16 secondary, controls `md` 28.
  - Inset dividers to the text column.
  - Move explanatory prose to a group footer, and per-server diagnostics on
    Providers into a disclosure row.
  - Put font rows in a "Fonts" group.
- **Before:** `mocks/DS/DS-08-before.svg`
- **After:** `mocks/DS/DS-08-after.svg`
- **Effort:** M

### DS-09 — Icon set is not uniformly Solar Linear
- **Severity:** P1
- **Category:** iconography
- **Where:** `crates/ui/src/icons.rs:55-215` (registry comments: "Hand-drawn
  … in the Solar Linear style", "the embedded set has no …");
  `crates/ui/assets/icons/{close,plus,terminal,window-*}.svg` (16 px canvas,
  stroke 1.25), `check.svg` (16 / 1.6), `pause.svg` (stroke 2),
  `chart.svg` / `pin.svg` (1.6), `settings-minimalistic.svg` (square caps),
  `moon.svg` / `key-minimalistic.svg` (outline-expanded fills),
  `stop.svg` (10 px canvas). Inventory: `scratchpad/ds/icon_inventory.tsv`.
- **What's wrong:**
  - The project rule is to fetch every glyph from Solar Linear via
    `@solar-icons/cli`. In practice:
    - ≈47 of 112 assets are hand-drawn or ported.
    - 4 are mirrored or rotated copies.
    - Only ≈57 were fetched.
  - Several comments justify a hand-drawing because "the embedded set" (the
    local subset) lacks a glyph: info-circle, up chevron, arrow-down, expand,
    bell, star, home. The full catalog is searchable through the CLI.
  - At the common 16 px size, Solar glyphs render 1.0 px strokes. `close`,
    `plus` and `terminal` render 1.25 px (+25%), `check` 1.6 px (+60%),
    `pause` 1.33 px (+33%), and `chart`/`pin` 1.07 px with different
    geometry.
- **Why it hurts the premium feel:** A toolbar where the ✕ and + look bolder
  than the folder and gear beside them breaks the "one hand drew every
  glyph" quality that SF Symbols gives Apple apps.
- **Proposed fix:**
  - Re-fetch every non-brand glyph with
    `npx -y @solar-icons/cli search <term> --style linear`, then
    `get <name> --style linear --json`, in the house format (1em, 24
    viewBox, `currentColor`, stroke 1.5, round caps and joins).
  - Use filled glyphs only for state (favourited star, stop), taken from
    Solar Bold.
  - Where Solar truly lacks a glyph, redraw it on the 24 grid at 1.5 and tag
    it `custom` in `icons.rs`.
  - Extend the existing `every_registered_icon_loads_and_parses` test to
    assert viewBox and stroke width for non-brand assets.
  - Brand marks and the polychrome Symbols file icons stay as they are.
- **Before:** `mocks/DS/DS-09-before.svg`
- **After:** `mocks/DS/DS-09-after.svg`
- **Effort:** M

### DS-10 — 17 corner radii, colliding names, non-concentric nesting
- **Severity:** P1
- **Category:** consistency
- **Where:** 296 literal `rounded(px(N))` calls (top: `shell.rs` 36,
  `settings/appearance.rs` 32, `transcript.rs` 21, `shell/usage.rs` 20).
  `crates/ui/src/theme.rs:1302-1306` (`BUBBLE` 16 / `PANEL` 10 / `CONTROL`
  6). `popover.rs:302` `CARD_RADIUS` = 12 vs `composer/goals.rs:77`
  `CARD_RADIUS` = 16. `queue.rs:101` `PANEL_RADIUS` = 16. Settings
  `section_card` r12 (`widgets.rs:293`) holds `row_tile` r10 at a 14/20 pt
  inset (`widgets.rs:321`).
- **What's wrong:**
  - 17 distinct radii are in use (1, 2, 3, 3.5, 4, 4.5, 5, 5.5, 6, 7, 8, 9,
    10, 12, 14, 16, 26), and controls of the same height use 5, 6, 7, 8 and
    9 (DS-02).
  - `Theme::PANEL_RADIUS` (10) is used once, while settings cards hard-code
    12.
  - Nested surfaces ignore the concentric rule. The Settings tile is nearly
    as round as its card despite the 14 pt inset, so it looks puffy.
  - Popover rows do follow the rule (`MENU_ITEM_RADIUS = CARD_RADIUS -
    CARD_INSET`, `popover.rs:309`), which shows the intended pattern.
- **Why it hurts the premium feel:** Apple's continuous-corner hierarchy
  (small controls tight, containers rounder, insets concentric) is subtle
  but everywhere. Random radii make edges look slightly off at every scale.
- **Proposed fix:**
  - Add `theme::radius::{XS 4, SM 6, MD 8, LG 12, XL 16, COMPOSER 26}` plus
    `full`, and `radius::inner(outer, inset)` with a floor of 4.
  - Map controls of 24 pt and under to SM, 28–32 pt to MD, cards, popovers
    and settings groups to LG, and dialogs, palette, bubbles and sheets to
    XL.
  - Delete the duplicate constants. Settings tile becomes 28 at r7.
- **Before:** `mocks/DS/DS-10-before.svg`
- **After:** `mocks/DS/DS-10-after.svg`
- **Effort:** M

### DS-11 — Spacing is half off the 4-pt grid
- **Severity:** P2
- **Category:** spacing
- **Where:** 1,432 literal padding/gap/margin values (tallies
  `scratchpad/ds/spacing_*.txt`). Top files: `shell.rs` 159,
  `settings/appearance.rs` 123, `pickers.rs` 93, `shell/usage.rs` 81.
  Tokens at `crates/ui/src/theme.rs:1308-1311`.
- **What's wrong:**
  - Padding uses 32 distinct values, gaps 20 and margins 28.
  - Only 53% of paddings and 46% of gaps sit on the 4-pt grid. 12–13% are
    odd numbers (3, 5, 7, 9, 11, 13, 15, 21) that put edges on half-pixels
    at 1× and drift visibly against neighbours.
  - The four `SPACE_*` tokens cover 8% of uses.
- **Why it hurts the premium feel:** Rhythm is felt rather than seen.
  5/6/7 pt gaps between equivalent icon+label pairs make rows look
  hand-placed.
- **Proposed fix:** Add the `theme::space` scale: 2, 4, 6, 8, 10, 12, 16, 20,
  24, 32, 40, 48 and 64 (names `space.0.5` … `space.16`; 6 and 10 are the
  allowed half-steps for control interiors). Apply the snap rules in the
  after-mock. Keep `TEXT_STACK_GAP` (1 pt) as a named optical nudge. Add a
  CI grep that flags new literal `px()` values in `p*`/`gap*`/`m*` calls.
- **Before:** `mocks/DS/DS-11-before.svg`
- **After:** `mocks/DS/DS-11-after.svg`
- **Effort:** L (mechanical)

### DS-12 — 17 icon sizes; no icon tokens or icon+label helper
- **Severity:** P2
- **Category:** iconography · alignment
- **Where:** `icon(X).size(px(N))` across the crate (tally
  `scratchpad/ds/icon_size_by_role.txt`). `crates/ui/src/icons.rs:256`
  (`icon()` takes any size). Examples: `PLUS` at 11/12/13/14/16, `CHECK` at
  10/12/13/14/16, `ALT_ARROW_DOWN` at 11/12/13/14, `CLOSE` at 9/11/12.
- **What's wrong:**
  - Icons render at 17 sizes. The same glyph changes size by context with no
    rule, and icon-to-label gaps vary between 4, 5, 6, 7 and 8 pt.
  - There is no helper for optical centring of a glyph against a text line,
    so each site nudges by hand.
  - Below 13 px, Solar's 1.5/24 stroke renders under 0.81 px (0.69 px at
    11), so small line icons go grey and soft.
- **Why it hurts the premium feel:** SF Symbols pair a symbol scale with the
  text style. Ad-hoc sizes make glyphs look heavier or lighter than their
  labels from row to row.
- **Proposed fix:**
  - Add `IconSize { Xs 12, Sm 14, Md 16, Lg 20 }`, each paired with a
    `TextStyle` and a gap (4/6/6/8).
  - Add `ui::label(icon, text, TextStyle)`, which centres the glyph on the
    text's x-height.
  - Line icons start at 14; 12 is only for filled status glyphs.
  - One size per role: chevrons 14, close 14, check 14 in menus and 16 in
    rows.
- **Before:** `mocks/DS/DS-12-before.svg`
- **After:** `mocks/DS/DS-12-after.svg`
- **Effort:** M

### DS-13 — Secondary text has no defined tiers (two greys on one page)
- **Severity:** P2
- **Category:** color/contrast · consistency
- **Where:** `crates/ui/src/settings/widgets.rs:355-360` (`meta_line`:
  `text_muted.opacity(0.65)` in dark, bypassing the `readable_*` floor
  mandated by `docs/theme-system.md`). `crates/ui/src/settings/appearance.rs:3729`
  (font-row description: `text_muted`). `crates/ui/src/theme.rs:837`
  (`readable_muted(dark_opacity)`). `theme.rs:1606-1608` (`text_muted`
  #a1a1a1, `text_dim` #989898, `text_faint` #737373). Shot:
  `shots/design-system/appearance-dark-font-rows-vs-card-rows.png`.
- **What's wrong:**
  - On the Appearance page in Luna Dark, descriptions of the same rank
    render **#c9c9cc** (font rows) and **#868688** (card rows), measured as
    peak glyph colour. That is a 1.9× luminance gap for the same role.
  - Secondary text is chosen at the call site from: `text_muted` (505),
    `text_faint` (106), `text_dim` (31; within 9/255 of `text_muted`),
    `readable_muted` with 10 different alphas (0.45–0.9), `text.opacity(…)`
    (33) and `text_muted/faint.opacity(…)` (11).
  - Light mode happens to be uniform (#445064), because the light branch
    ignores the alpha.
- **Why it hurts the premium feel:** Apple's label / secondaryLabel /
  tertiaryLabel ladder is why their hierarchy looks deliberate. Here,
  hierarchy shifts from row to row.
- **Proposed fix:** Add `Theme::label(LabelTier::{Primary, Secondary,
  Tertiary, Quaternary})`. Each tier is pre-floored once per surface stack
  (secondary = `text_muted`, tertiary = `text_faint` floored to 4.6:1,
  quaternary = disabled and exempt). Replace `readable_muted(α)`,
  `text_dim` and raw `.opacity()` on text. `meta_line` uses
  `label.secondary`.
- **Before:** `mocks/DS/DS-13-before.svg` (+ shot)
- **After:** `mocks/DS/DS-13-after.svg`
- **Effort:** M

### DS-14 — Fills and hairlines are 27 raw alphas, untinted by the theme
- **Severity:** P2
- **Category:** color/contrast · consistency
- **Where:** `crates/ui/src/theme.rs:2208-2250` (`ink`, `hairline` and
  `wash` are pure white in dark and black in light). 150 `ink` + 61 `wash`
  + 45 `hairline` calls with 27 distinct alphas; 435 `.opacity(literal)`
  calls with 50 values (`scratchpad/ds/ink_alphas.txt`,
  `opacity_values.txt`). Theme roles `element_hover` (23 uses, plus 53 via
  `glass_hover()`), `border` (157) and `border_strong` (58).
- **What's wrong:**
  - Two parallel systems paint the same roles. Theme-authored roles
    (`element_hover`, `border`) coexist with call-site white or black
    alphas.
  - On a tinted theme (for example Nord, `#2e3440`), an `ink(0.08)` hover
    next to the theme's own hover produces two different tints for one
    role in the same list.
  - Border alphas alone come in 12 values (0.035–0.14, plus 0.6).
- **Why it hurts the premium feel:** Imported themes are a headline feature.
  Neutral washes flatten their character and make some states look grey
  and dirty.
- **Proposed fix:** Add `Theme::fill(FillTier::{Quaternary 4%, Tertiary 6%,
  Secondary 10%, Primary 16%})` and `Theme::separator(strong)` (8% / 14%).
  Derive them from `theme.text`, so they carry the palette's tint, and floor
  them per surface like the text helpers. Make `ink`, `wash` and `hairline`
  private to `theme.rs`. `element_hover` becomes `fill.tertiary`.
- **Before:** `mocks/DS/DS-14-before.svg`
- **After:** `mocks/DS/DS-14-after.svg`
- **Effort:** M

### DS-15 — Motion catalog fragmented: 16 durations, 6 curves, no spring
- **Severity:** P2
- **Category:** motion
- **Where:**
  - `crates/ui/src/motion.rs:296-409` (catalog).
  - Local specs: `transcript.rs:120` (`TOOL_FOLD` 140), `:127`
    (`TOOL_ROW_REVEAL` 360), `:131` (480), `shell.rs:785` (`RESORT` 260),
    `composer.rs:8547` (240), `shell/workbench/render.rs:43-44` (120/160),
    `shell/workbench.rs:256-257` (150/170), `popover.rs:1305-1307`.
  - Scale limitation noted at `motion.rs:24-27`.
- **What's wrong:**
  - The one concept "disclosure/collapse" runs at four timings: 140
    (`TOOL_FOLD`), 170 (`COLLAPSE_MS`), 180 (`COLLAPSE`) and 200
    (`CHEVRON`).
  - Six bezier curves overlap: `EASE_OUT_EXPO`, `EASE_OUT_QUINT` and
    `EASE_TAILWIND` are three near-identical decelerations.
  - Nothing is spring-driven, although expand/collapse is exactly where
    Apple uses springs.
  - Menus and dialogs cannot scale (no div transform at the pinned GPUI
    revision), so they slide and fade instead of the macOS "pop".
  - Some catalog entries (`FADE_IN`, `FADE_QUICK`, `MENU_IN`, `DIALOG_IN`)
    are used only through helpers, while 9 specs are defined ad hoc.
- **Why it hurts the premium feel:** Motion that shares a rhythm feels
  physical. Four different speeds for "open a section" feel unrelated.
- **Proposed fix:**
  - Add `motion::{MICRO 120, QUICK 160, STANDARD 240, EMPHASIZED 360}` with
    three curves: standard (0.2, 0, 0, 1), decelerate (0.16, 1, 0.3, 1) and
    accelerate (0.4, 0, 1, 1).
  - Add `motion::DISCLOSURE` as a spring with response 0.32 s and damping
    0.86, and use it for tool folds, thinking blocks, chevrons, settings
    disclosures and pane collapse. Exits run at 0.75× the enter time.
  - Migrate the local consts. Track a GPUI div scale transform as an
    upstream follow-up, so menus can get scale 0.97 → 1.
- **Before:** `mocks/DS/DS-15-before.svg`
- **After:** `mocks/DS/DS-15-after.svg`
- **Effort:** M

### DS-16 — Five ways to show "selected"; no shared segmented control
- **Severity:** P2
- **Category:** consistency · states
- **Where:** `crates/ui/src/settings/appearance.rs:1374` / `:1422` / `:1500`
  (separate pills, accent wash + border, h30/h28 r7);
  `crates/ui/src/settings/general.rs:270-318` (joined track r9 p2 with a
  page-bg thumb r7); `appearance.rs:1913-1916` (accent swatch: 2 pt
  `border_b_2` underline); option cards (`widgets.rs:241`, accent hairline);
  Providers harness tabs (filled lavender pill, see the mcp-inventory shot).
- **What's wrong:**
  - Single-choice controls show selection five different ways on adjacent
    pages: accent wash + border, neutral thumb, underline, accent hairline,
    and filled lavender.
  - Two segmented designs exist: separate pills on Appearance and a joined
    track on General.
  - The accent swatch's underline reads like a tab indicator, not a
    selection.
- **Why it hurts the premium feel:** NSSegmentedControl and the accent-
  colour picker in System Settings each look the same everywhere. Consistent
  selection affordances are a large part of "native".
- **Proposed fix:** Add `ui::Segmented` (track r8, p2, sliding thumb r6
  using `fill.primary`, labels footnote 12, height `md` 28) for every 2–5
  option choice, including the harness tabs. Add `ui::selection_ring`
  (2 pt accent ring at a 2 pt offset, radius = inner + 4) for swatches and
  option cards. Reserve accent for rings and focus.
- **Before:** `mocks/DS/DS-16-before.svg`
- **After:** `mocks/DS/DS-16-after.svg`
- **Effort:** S

### DS-17 — Seven focus treatments; most clickables are mouse-only
- **Severity:** P2
- **Category:** states · interaction
- **Where:** 67 direct `focus_visible` sites. Variants:
  - `theme::focus_ring` (`theme.rs:2382`, inset 2 pt; ×23 direct, plus 46
    via `AccessibleButton`, `lib.rs:147`).
  - `border_color(border_strong)` ×8.
  - `border_color(accent)` ×11 on 1 pt borders.
  - `bg(accent.opacity(0.18))` ×5.
  - `bg(surface_raised)` ×2, `card_selected_bg` ×2.
  - `border_2().border_color(accent)` ×2, which grows the box by 2 pt.
  - `popover::button_frame` swaps a transparent 1 pt border
    (`popover.rs:1149-1150`).
- **What's wrong:** Keyboard focus looks different on nearly every surface.
  The `border_strong` variant is a 14% neutral hairline that is hard to see
  and not the accent. There are 556 `.on_click()` handlers against about 144
  tab stops (98 `tab_index` + 46 `accessible_button`), so most clickable
  chrome cannot be reached from the keyboard.
- **Why it hurts the premium feel:** A single crisp focus ring is part of
  the Mac keyboard experience and of accessibility review. Seven styles
  look accidental.
- **Proposed fix:** Fold `AccessibleButton` into `ui::interactive(id,
  label)`: role, aria label, tab stop, `theme::focus_ring`, hover fill and
  press. Make it the only way to attach `on_click` to chrome. Delete the
  per-site `focus_visible` closures. Add a lint: an element with
  `on_click` and no tab stop must opt out explicitly.
- **Before:** `mocks/DS/DS-17-before.svg`
- **After:** `mocks/DS/DS-17-after.svg`
- **Effort:** M

### DS-18 — Sub-11 pt text in everyday chrome
- **Severity:** P2
- **Category:** typography · color/contrast
- **Where:** `text_size(ui_rems(N))` with N = 9 (×3), 9.5 (×3), 10 (×57)
  and 10.5 (×31). Examples: `crates/ui/src/shell/usage.rs:1288` (usage
  status, 9), `:3049` (usage error, 9), `shell/command_palette.rs:732` and
  `:848` (row metadata and section header, 9.5), `history.rs:4491` (9.5).
  Top files: `shell/usage.rs` 13, `pickers.rs` 10, `shell.rs` 7,
  `history.rs` 5.
- **What's wrong:**
  - 94 sites set readable copy below 11 pt, including error messages at
    9 pt, mostly in muted colour on raised fills.
  - At the 12 px interface setting they render at 6.75–7.9 pt.
  - Contrast passes (DS overview), but small size and muted colour compound.
- **Why it hurts the premium feel:** macOS's smallest text style is 10 pt
  and readable copy floors at 11. Tiny grey text reads as "dense developer
  tool", not "Apple".
- **Proposed fix:** Floor all readable copy at `caption` 11/14. Restrict
  `caption2` 10/13 to glyph-like text (kbd hints, counts, uppercase badges).
  Delete 9, 9.5 and 10.5 from use. Errors use `footnote` 12 in
  `label.primary` with the danger tone.
- **Before:** `mocks/DS/DS-18-before.svg`
- **After:** `mocks/DS/DS-18-after.svg`
- **Effort:** S

### DS-19 — 16–18 pt click targets
- **Severity:** P2
- **Category:** sizing · interaction
- **Where:** `crates/ui/src/comment_ui.rs:113` (comment remove, 16×16),
  `history.rs:1781` (search close, 16), `files/preview.rs:3009` (16),
  `shell/workbench/render.rs:1750` (failure dismiss, 18),
  `browser/view.rs:471` (go, 18), `composer.rs:8313` (18),
  `shell.rs:13662` (18).
- **What's wrong:** Several icon-only controls have a hit box equal to a 16
  or 18 pt visual box. Toolbar controls (`surface_chrome::CONTROL_SIZE` 24)
  are fine; these one-offs are not.
- **Why it hurts the premium feel:** Missed clicks on tiny ✕ buttons are a
  daily irritation. Apple keeps a comfortable hit region even when the
  glyph is small.
- **Proposed fix:** Use `ui::IconButton::sm`: a 24×24 hit box (28 in
  toolbars), `icon.sm` 14 glyph, `radius.sm`, and the shared hover fill.
  Where space is tight, extend the hit box with a negative margin instead
  of shrinking it.
- **Before:** `mocks/DS/DS-19-before.svg`
- **After:** `mocks/DS/DS-19-after.svg`
- **Effort:** S

### DS-20 — Six tooltip delays, no warm-up, no fade
- **Severity:** P3
- **Category:** interaction · motion
- **Where:**
  - `tooltip_show_delay` of 250 (`history.rs:3653`), 280
    (`badges.rs:64` `HOVER_DELAY`), 300 (×4), 350 (×25: `shell.rs`,
    `queue.rs`, `history.rs`, `spaces.rs` …) and 400
    (`pickers.rs:520` `CONTROL_TOOLTIP_DELAY`, ×13) ms.
  - The GPUI default of 500 ms applies elsewhere
    (`vendor/zui/crates/gpui/src/elements/div.rs:49`).
  - Card surface: `popover.rs:323`.
- **What's wrong:**
  - Neighbouring controls wait different times: composer pickers 400 ms,
    queue buttons beside them 350 ms.
  - There is no warm-up, so moving along a toolbar re-waits at every button.
  - Tooltips pop in without a fade.
- **Why it hurts the premium feel:** It is a small thing, but tooltips that
  arrive at random speeds make the chrome feel inconsistent.
- **Proposed fix:** Add `ui::tooltip(text)`: 500 ms cold, 0 ms warm (within
  1.5 s of the last tooltip), and a `motion::MICRO` fade. Remove the literal
  delays.
- **Before:** `mocks/DS/DS-20-before.svg`
- **After:** `mocks/DS/DS-20-after.svg`
- **Effort:** S

### DS-21 — One glyph, many meanings in Settings
- **Severity:** P3
- **Category:** iconography
- **Where:** `crates/ui/src/settings/appearance.rs:3465` (Accent color),
  `:3639` (Background effect), `:3898` (Diff colors) and `:3914` (Reset)
  all use `TUNING`. The Settings rail uses `TUNING` for Appearance
  (`shell.rs:8450`) right below `SETTINGS_MINIMALISTIC` (another sliders
  glyph) for General (`shell.rs:8446`). `WRAP_TEXT` is used for both Chat
  code wrapping and Diff wrapping (`:3770`, `:3792`).
- **What's wrong:** The sliders glyph stands for colour, effect, reset and
  the page itself. Two slider glyphs sit adjacent in the rail.
- **Why it hurts the premium feel:** Icons that name their subject make a
  pane scannable. Generic repeats turn the tiles into decoration.
- **Proposed fix:** One glyph means one thing per screen. Use Solar Linear
  subjects (search via CLI): palette (accent), magic-wand (effect, already
  in assets), split-columns (diff colours), restart (reset), sun/moon or
  palette (Appearance rail) and settings-gear (General rail).
- **Before:** `mocks/DS/DS-21-before.svg`
- **After:** `mocks/DS/DS-21-after.svg`
- **Effort:** S

### DS-22 — Page, group and row titles look alike
- **Severity:** P3
- **Category:** typography
- **Where:** `crates/ui/src/settings/widgets.rs:169` (`page_header` 16
  semibold), `:203` (`field_label` 13 medium), `:338` (`row_title` 13
  medium); Providers card titles 13 semibold (harnesses).
- **What's wrong:**
  - Three of four heading levels are 13 pt, separated only by weight 500 vs
    600.
  - The page title (16) is barely above body.
  - The first group on Appearance repeats the page name ("Appearance" →
    "Appearance").
- **Why it hurts the premium feel:** A clear three-level hierarchy (pane
  title, group, row) is what makes System Settings scannable at a glance.
- **Proposed fix:** Use `title1` 20/25 semibold for the page title, body 13
  semibold in `label.secondary` for group headers, and body 13 medium plus
  footnote 12 for rows. Group names describe content ("Themes", "Fonts",
  "Code & diffs"). Provider cards become groups with the same header.
- **Before:** `mocks/DS/DS-22-before.svg`
- **After:** `mocks/DS/DS-22-after.svg`
- **Effort:** S

## Proposed unified tokens

All UI values are in pt at the 16 px default and scale through `ui_rems`.
Code and terminal sizes keep their own absolute settings.

### Type scale: `typography::TextStyle`

| Token | Size / line | Weight | Use |
| --- | --- | --- | --- |
| `caption2` | 10 / 13 | 400 | kbd hints, counts, uppercase badges only |
| `caption` | 11 / 14 | 400 · 500 | chrome labels, meta, chips, sidebar secondary |
| `footnote` | 12 / 16 | 400 | row descriptions, tool rows, menu secondary |
| `body` | 13 / 18 | 400 · 500 | default UI text, row titles, buttons (md/lg) |
| `prose` | 14 / 21 | 400 | chat markdown, composer input |
| `title3` | 15 / 20 | 600 | card and dialog titles |
| `title2` | 17 / 22 | 600 | section titles |
| `title1` | 20 / 25 | 600 | page titles |
| `display` | 26 / 32 | 700 | empty states, hero |

Rules: set size, line height and weight together via `text_style()`. There
are no half-point sizes. `BOLD` is used only for `display`.

### Spacing: `theme::space`

| Token | 0.5 | 1 | 1.5 | 2 | 2.5 | 3 | 4 | 5 | 6 | 8 | 10 | 12 | 16 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| pt | 2 | 4 | 6 | 8 | 10 | 12 | 16 | 20 | 24 | 32 | 40 | 48 | 64 |

Rules:
- 6 is the icon↔label gap and 10 the control's horizontal padding.
- Settings: group gap 24, header → card 8, row px 16, row gap 12.
- `TEXT_STACK_GAP` (1) stays as the only sub-grid optical nudge.

### Radius: `theme::radius`

| Token | pt | Use |
| --- | --- | --- |
| `XS` | 4 | inline code, kbd, checkbox, progress bars |
| `SM` | 6 | controls ≤ 24 pt, chips, icon buttons |
| `MD` | 8 | controls 28–32 pt, menu rows, inputs |
| `LG` | 12 | cards, popovers, settings groups, tooltips |
| `XL` | 16 | dialogs, command palette, message bubbles, sheets |
| `COMPOSER` | 26 | composer pill only |
| `full` | – | pills, switches, avatars, dots |

Rule: `radius::inner(outer, inset) = max(outer − inset, 4)` for every nested
surface. The Settings tile is 28 pt at r7.

### Control sizes: `ui::ControlSize`

| Size | Height | Radius | Label | Padding x | Icon |
| --- | --- | --- | --- | --- | --- |
| `sm` | 24 | SM 6 | caption 11 | 8 | 14 |
| `md` | 28 | MD 8 | footnote 12 | 10 | 14 |
| `lg` | 32 | MD 8 | body 13 | 12 | 16 |

- Variants: primary, secondary, ghost (6% rest fill) and danger.
- `Select` and `Segmented` share these heights.
- Switches: 38×22 regular or 26×15 small.
- Minimum hit area: 24×24.

### Colour roles (additions)

- **Labels:** `label.primary` / `secondary` / `tertiary` / `quaternary`,
  each pre-floored once per surface stack.
- **Fills:** `fill.quaternary` 4%, `tertiary` 6% (hover), `secondary` 10%
  (pressed), `primary` 16% (selected neutral, tracks). All derive from
  `theme.text`.
- **Separators:** `separator` 8%, `separator.strong` 14%.
- **Selection:** `accent.wash` (selected), `accent` ring (selection and
  focus only).

### Motion: `motion::`

| Token | Value | Use |
| --- | --- | --- |
| `MICRO` | 120 ms, standard | hover, press, tooltip fade |
| `QUICK` | 160 ms, standard | switch track, segment thumb, selection ring, chevron |
| `STANDARD` | 240 ms, decelerate | menus and popovers in, tab slide, toasts |
| `EMPHASIZED` | 360 ms, decelerate | panes, sheets, new-thread, route changes |
| `DISCLOSURE` | spring, response 0.32 s, damping 0.86 | tool folds, thinking, settings disclosures, pane collapse, switch knob |
| curves | standard (0.2, 0, 0, 1) · decelerate (0.16, 1, 0.3, 1) · accelerate (0.4, 0, 1, 1) | — |

Rules:
- Exits run at 0.75× the enter time.
- Reduce motion (system-observed, DS-04) gives a 120 ms opacity crossfade
  and static loaders.
- Tooltip timing: 500 ms cold, 0 ms warm.

### Icons: `icons::IconSize`

| Token | px | Pairs with | Gap |
| --- | --- | --- | --- |
| `Xs` | 12 | caption2 / caption (filled status glyphs only) | 4 |
| `Sm` | 14 | caption / footnote: toolbars, chips, menus | 6 |
| `Md` | 16 | body: buttons, settings tiles, rows | 6 |
| `Lg` | 20 | title3+: titlebar, empty states | 8 |

Source rule: Solar Linear only (24 viewBox, 1.5 stroke, round caps and
joins, `currentColor`, 1em). Use Solar Bold only for state. Brand marks and
Symbols file icons are exempt. An `icons.rs` test asserts the format.

## Top 5 fixes with the biggest premium payoff

1. **Shared control kit plus `ui::interactive`** (DS-02, DS-05, DS-16,
   DS-17, DS-19). One Button / IconButton / Select / Segmented in three
   sizes, with an animated hover fill and a single focus ring. This removes
   the most visible "assembled" signal: mixed heights, radii and hover feel.
2. **Type scale with paired leading** (DS-01, DS-18, DS-22). Nine
   `TextStyle`s replace 17 sizes and φ leading. Every row tightens, labels
   align, and the page hierarchy appears.
3. **Settings row and group anatomy** (DS-03, DS-07, DS-08). One
   `settings::row` (52/40 pt, two-column, reflowing) and `settings::group`
   (24/8 spacing). This fixes the overlap bug, makes Settings 30% denser,
   and gives it a System Settings feel across all 17k lines.
4. **Motion tokens, system Reduce motion and moving thumbs** (DS-04, DS-06,
   DS-15). Four durations, three curves and a disclosure spring. Switches
   and segments travel instead of teleporting, and Reduce motion is finally
   honoured.
5. **Solar-only icon pass with size tokens** (DS-09, DS-12, DS-21).
   Re-fetch about 50 glyphs, normalise them to 1.0 px at 16, and add four
   icon sizes paired with text. Toolbars and rows gain the "one hand drew
   everything" coherence.
