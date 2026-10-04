# Appearance audit

Requested by Iulian on 2026-10-04, after the unified-frame color changes: a
deep review of appearance settings and every UI family for background, text,
icon and title readability. Benchmark: WCAG 2.2 AA —
[1.4.3 Contrast (Minimum)](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html)
(normal text ≥ 4.5:1, genuinely large text ≥ 3:1) and
[1.4.11 Non-text Contrast](https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast.html)
(meaningful icons, indicators, focus visuals and control boundaries ≥ 3:1),
with Apple's Human Interface Guidelines on accessible color as the platform
reference. Contrast is one criterion: nothing here claims full WCAG
conformance, which also depends on keyboard access, text resizing,
non-color cues, motion and assistive-technology semantics.

## Status

**2026-10-04 follow-up:** the persistent-fork milestone replaces selection
underlines with rounded neutral/accent tab fills and removes both horizontal
header rules. The current matrix passes all 810 combinations in
`target/qa/persistent-fork-threads/appearance-attempt-2/`. Workbench
`ui-attempt-8` passes all 144 rendered pairs, with minima 4.68:1 for text and
3.11:1 for the measured non-text role. Native dark/light confirmation is in
that milestone's `native/n08-final-tab-fill-dark.png` and
`native/n09-final-tab-fill-light.png`. The earlier measurements below remain
the evidence for the original color-system milestone.

| Item | State |
| --- | --- |
| Fixes in `crates/ui` | F1–F27, compiled and measured. |
| Matrix measurement | `appearance-audit` example, run by Codex: **810 combinations** (30 variants × 3 surface preferences × 9 accent/diff settings). Writes `contrast.json`, `contrast.md`, `appearance-audit.pass` or `failure.txt`, and exits 1 on any failure after writing them. |
| Measured results | Attempt 1: 4,210 failing pair instances. Attempt 2: 240 in four element types, plus a hue defect. **Attempt 3: PASS — 0 failures, exit 0** (Results). No ratio here is estimated. |
| Rendered-pixel measurement and captures | `chat-workbench-fixture`: w21–w26 frame states (both sidebars open, each and both collapsed, mid-reflow, after a sidebar resize), w27 Related/Changes/Files top rows in dark and light (six captures), w28 eight representative theme/surface pairs with text, secondary text and the focus edge measured against rendered pixels (`renderedContrast` in `readback.json`). **Fixture run 16 on the final source: PASS** (exit 0, `chat-workbench.pass`, all 144 rendered pairs at their floors; Results). Run 11 passed earlier with identical rendered figures. |

Evidence: `target/qa/super-chat-workbench/appearance-attempt-{2,3}/`
(`contrast.json`, `contrast.md`, `failure.txt` or `appearance-audit.pass`,
`hue-review.json`) for the matrix, and
`target/qa/super-chat-workbench/ui-attempt-16/` (`readback.json`,
`readback-progress.json`, `chat-workbench.pass`, w01–w28 captures) for the
rendered checks.

## Method

Colors are measured as composited, never by token name:

1. **Surfaces.** The shell frost (`Theme::glass`) is flattened over the
   desktop. A translucent surface's real backdrop is unknown, so both
   extremes are measured and the lower ratio counts: white behind dark glass
   and black behind light glass (adverse), and the opposite (benign). Every
   surface a role is painted on is then composited in paint order: hover and
   selected washes on chrome, the workbench canvas (`Theme::canvas_fill`)
   on chrome, the markdown code plate on the canvas, raised chips, floating
   cards (`glass_overlay`), input plates (`input_glass_bg`), settings cards,
   dialogs over the scrim, accent and diff washes. Where a foreground sits on
   more than one translucent layer, every layer combination it can be
   painted over is measured: a Main badge on its accent wash over a resting,
   hovered (5%) or active (9%) tab chip; a check on a selected menu row,
   resting or hovered.
2. **Foregrounds.** Each foreground uses its real call-site value: the
   readable helper with the opacity the component passes
   (`readable_muted(0.5)` for a sidebar subline, `for_popup()` roles in
   menus, `readable_icon(text_muted × 0.45)` for the unfocused tab
   indicator), so
   faded secondary text is measured as painted. Where the production
   painter is a function, the audit calls it rather than re-deriving it (see
   UI families below).
3. **Ratio.** WCAG relative luminance of the flattened foreground against
   the flattened surface (`theme::contrast_ratio(flatten(fg, bg), bg)`),
   compared **unrounded and strictly**: a pair passes only when the measured
   ratio is ≥ 4.5 (text) or ≥ 3.0 (non-text). WCAG states the thresholds are
   not rounded; 2.999:1 fails 3:1. Reports show the real ratio. Derived paint
   colors aim at 4.6:1 and 3.1:1 (`TEXT_CONTRAST_TARGET`,
   `NON_TEXT_CONTRAST_TARGET`) so f32 compositing and 8-bit rendering cannot
   push a floored color across the line; the requirement itself stays
   4.5 / 3.0.
4. **Matrix.** Every built-in variant (30) × surface preference (3) ×
   accent (theme default + 7 presets) plus the theme-default accent under the
   alternate diff scheme — 30 × 3 × 9 = 810 combinations — through the
   production `appearance::set_mode/set_theme/set_accent/set_surface/
   set_diff_color_scheme` path, so each value comes from the same
   `Theme::install_selection` → `from_variant` derivation the app paints.
5. **Independent checks.** (a) Every cached floor is re-verified against
   freshly computed, uncached surfaces, for the installed palette and for a
   hot-reloaded copy with changed foreground roles (catches a stale cache
   key). (b) The workbench fixture samples rendered pixels (canvas, left and
   right chrome) from the same frame it saves as a screenshot and measures
   text, secondary text and the focus edge against those pixels over both
   desktop extremes. (c) The floor helpers' paint-time cost is timed, and a
   160×50 terminal frame is painted cold and warm. (d) Hue preservation:
   each derived accent, status, diff-marker and terminal ANSI tone is
   compared with its source tone as painted, on both desktops, for every
   combination, and must stay within 15° (failure case 15).
6. **Derivation.** A foreground that misses its floor first gains opacity,
   then moves in lightness only (hue and saturation kept), toward the side
   the text color reads on and then the other; only if no lightness of that
   hue reads does it fall back to neutral white or black (F25). Stored
   themes and preferences are never rewritten.

## Settings coverage

| Setting | Values | Covered by | Notes |
| --- | --- | --- | --- |
| Appearance mode | System, Light, Dark | Matrix (Light, Dark); System resolves to one of them | |
| Built-in themes | 30 variants in 19 families (Luna, VS Code, Catppuccin, Tokyo Night, Dracula, GitHub, Ayu, Gruvbox, Rosé Pine, Nord, One Dark Pro, Atom One Dark, Night Owl, Winter is Coming, Palenight, SynthWave '84, Shades of Purple, Cobalt2, Andromeda) | Matrix: all; rendered pixels: 8 representative | |
| Custom themes (imported, linked, editable) | Arbitrary palettes | Same `from_variant` derivation and floors | **Limit:** user files are not in the built-in matrix. The same derivation runs at install, but no custom palette was measured, and an extreme palette can reach the neutral white/black fallback. |
| Accent | Theme default, Luna, Orange, Amber, Green, Cyan, Blue, Pink | Matrix: all, every pair at the same strict floors | Pair rows are written for the theme-default accent; other accents add rows only when they fail, so `contrast.md` minima are default-accent minima. Attempt 3 recorded no accent failure. |
| Surface (glass) | Theme default, Frosted, Opaque | Matrix: all | Frost measured over both desktop extremes. |
| Opacity / vibrancy | Frost alpha (`GLASS_ALPHA` 0.80 dark / 0.94 light, raised by the contrast check), floating-card and input tint searches | Matrix, via the composited surfaces | |
| Diff colors | Red & green, Blue & orange | Matrix (every pair under both; diff rows recorded under both) | Blue & orange keeps additions/deletions distinguishable without red/green. Syntax floors are re-derived when the scheme changes. |
| Interface, code and terminal fonts and sizes | Families, sizes | Not a color input | Size changes can make text "large" (≥ 18.66 px bold / 24 px); the audit applies the stricter 4.5:1 everywhere instead. |
| Terminal colors (theme-owned) | Background, foreground, selection, cursor, 16 ANSI slots, per theme | Matrix: painted values through the renderer's own `terminal_paint_fg`, `terminal_cell_behind` and `terminal_cursor_glyph_fg`, including the full 17 × 17 palette matrix | See Terminal below. |
| New-thread composer background image and effect | Any image, effect | Matrix: all-white and all-black artwork at the strengths it is painted under the composer and its selectors | See New-thread artwork below. |
| macOS Reduce Transparency / Increase Contrast | System settings | **Not read by LUNA** | **Limit / follow-up:** Surface → Opaque is the in-app equivalent; reading the system flags is not implemented. |
| Reduced motion | System | Motion paths honor it (workbench exits, previews) | Not a contrast input. |

## UI families and measured pairs

Each pair below is in the audit (requirement in brackets). Exempt pairs are
measured and listed but never required.

Where a production painter exists as a function, the audit calls it
(`changes::diff_row_paint`, `diff_plain_text`, `diff_hunk_text`,
`terminal_paint_fg`, `terminal_cell_behind`, `terminal_cursor_glyph_fg`,
`Theme::readable_on/readable_on_stacks/readable_on_tab_badge/focus_ring/
selected_row_icon/on_danger/artwork_*_plate`) instead of re-deriving its
color.

- **Navigation**: row title on chrome, hover, selected [text]; project/branch
  subline on chrome and selected (`readable_muted(0.5)`) [text]; section label
  [text]; Child/Side kind badge on its 8% tint over chrome and over a selected
  row [text], its glyph [non-text]; status words Failed and Queued [text];
  disclosure chevron [non-text]; breadcrumb separator and disabled control
  glyph [exempt].
- **Workbench**: active tab title on its wash, inactive tab title on canvas,
  Main badge on the accent wash over a resting, hovered and active tab chip
  (`readable_on_tab_badge`) [text], its glyph on resting and active chips
  [non-text]; focused-group indicator and tab keyboard focus ring
  (`focus_ring`, on the active and hovered chip), unfocused indicator,
  drop-preview outline, approval and error glyphs [non-text]; pane context
  line, frame footer checkout/branch [text]. A selected tab is shown by its
  own rounded fill, with no underline or outline: the focused group's active
  tab by `tab_focused_fill` (a quiet accent tint) with `tab_title` [text] and
  the accent `tab_role_icon` [non-text] floored on it; other groups' by the
  neutral selected fill. The outline and underline pairs were replaced by
  these.
- **Related chats and tools**: toolbar title, Open badge on its 10% accent
  tint over chrome and a selected row [text], its glyph [non-text]; diff file
  path, ±N counts, hunk header (`text_faint` floored on the hunk band) [text];
  diff rows — code text, ± marker and line number on the row's own 5.5%
  added/deleted wash, in the side panel (chrome) and a central Diff tab
  (canvas) [text], the row's accent bar [non-text]; toolbar icons [non-text].
- **Transcripts, markdown and code**: body text, timestamps/metadata, inline
  code on its wash, code-block text, diagnostics [text]; 15 syntax roles on
  the code plate and on added/deleted diff rows (panel and tab) [text].
- **Composer and pickers**: input text, placeholder, hints, selected text on
  the composer pill as painted (the frosted sidebar tint, or the opaque input
  plate, over the canvas) and selected text in input fields [text]; focus
  border [non-text]; menu rows, secondary, hints on floating cards, the
  selected menu row (`card_selected_bg`) [text]; the selected-row check
  (`selected_row_icon`) resting and hovered, the picker hand-off check on
  resting and hovered rows, destructive menu glyph [non-text].
- **Menus, dialogs, popovers, settings**: dialog text and secondary, settings
  card text and secondary, raised chip text, primary/accent/destructive
  (`on_danger`) button labels on their fills [text]; dialog button focus
  border, settings selected checkmark [non-text]; empty-state illustration
  [exempt].
- **Errors, warnings, approvals**: danger/warning/success muted copy, the Run
  failed strip, and the raw-status call sites as they now paint (see the
  inventory below): theme-import failure and appearance error on their 8%
  washes over a settings card, file-watch and preview warnings on their
  washes over the canvas, history restore error on its wash over a menu,
  validation/warning/success/accent copy on cards, dialogs, menus and inputs
  [text].
- **States**: focus edge on chrome and canvas, accent focus ring on chrome,
  card and input [non-text]; hover and selected rows (above).
- **Terminal**: default foreground plain, selected, dim, dim-selected,
  inverse and inverse-selected, each on the background actually shown behind
  the glyph [text]; the block cursor [non-text]; the full palette matrix —
  default fg and the 16 ANSI slots on the default and 16 ANSI backgrounds
  (289 pairs), plain, selected and dim, plus the glyph redrawn above the
  focused cursor block on the cursor over that cell [text]; each authored
  ANSI slot and a program true-color sample [informational].
- **New-thread artwork**: composer text and placeholder on
  `artwork_composer_plate` over white and black artwork at half hero
  strength (the cutout's reveal); the selector labels on the opaque
  `artwork_selector_plate` over white and black artwork at full hero
  strength [text].

### Raw status/accent call-site inventory

Attempt 1 measured helpers; Codex's review found real components painting
copy in raw `theme.warning/danger/success/accent` (e.g. the Appearance
theme-import failure on `warning × 0.08`). Every `.text_color(theme.<status
or accent>…)` site in `crates/ui` was listed (59) and classified by what it
paints:

- **Meaningful icons (9)** → `readable_icon` (3:1): composer magic-wand,
  sidebar update arrow, delete/remove menu glyphs (shell, spaces), picker and
  settings checkmarks (pickers, appearance ×3), account update arrow.
- **Copy on its own wash (8)** → `readable_on(tone, wash, 4.6)`: Appearance
  import failure (warning 0.08) and error row (danger 0.08), file-watch error
  (warning 0.045), file-preview warnings (0.055, 0.045), history restore
  errors (danger 0.05).
- **Copy on reading surfaces (42)** → `readable_tone(tone, 1.0)`: validation
  messages, status words, usage warnings, MCP/Claude-settings errors,
  markdown/preview notices, browser error, history, pickers, spaces.

Also moved off raw tones: diff ±N counts (changes and transcript), diff row
markers/line numbers/accent bars and plain code (onto the row wash), hunk
header text. A second sweep (F27) found one multi-line form the first
missed — the picker setting-choice check, `if warning { theme.warning }` —
now `readable_icon`. No raw single-expression status/accent `.text_color`
remains (`rg` check at the freeze); fills, borders and washes keep their
tones. A text search cannot prove the absence of every indirect form (a tone
passed through a variable first), so the matrix's representative pairs and
the families above remain the acceptance evidence.

### Terminal

The panel paints `terminal.background` (thinned to 0.4 on glass) over the
shell chrome. Colors fall in two classes:

- **Theme-controlled** — the default foreground/background and the 16 ANSI
  slots. When both a cell's foreground and background are theme-controlled,
  the renderer applies a minimum contrast at paint (like VS Code's
  `terminal.integrated.minimumContrastRatio`): after inverse is applied, then
  dim, then against the background actually shown (`terminal_cell_behind`:
  the cell's default or ANSI background, then the selection over it when
  selected), the hue is kept and only its lightness moves, as far as 4.6:1
  needs. Dim text is still text and is floored too. When even
  the terminal foreground cannot read there (a light foreground on a bright
  ANSI yellow background), the floor falls back to white or black, whichever
  reads better, instead of a color known to fail.
- **Selection and cursor** — the theme's selection wash is thinned (never
  below 12% alpha) until the terminal foreground reads on it; the cursor
  block is floored at 3:1 on the panel. Under the focused (filled) cursor
  the glyph is redrawn above the block (`terminal_cursor_glyph_fg`), floored
  on the cursor over that cell's shown background, with the same guards.
- **Program-controlled** — the 256-color cube and ramp (16–255) and 24-bit
  true color. A program choosing these owns the pair; they paint exactly as
  asked and are outside Luna's chrome guarantees. The audit records a sample
  as informational.

Never adjusted, and proven in the audit (failure case 9): concealed cells
(SGR 8 — the emulator resolves them to foreground = background and they stay
invisible), block elements, box drawing and braille (graphics a program draws
with foreground ink: bars, borders, fills), and program-chosen colors. The
floor is memoized per resolved (foreground, opaque background, fallback)
triple; the audit times a 160×50 frame cold and warm.

### New-thread artwork

The hero paints the image at 0.84 strength on frosted themes (1.0 opaque),
feathered away around the composer by an alpha mask; inside the cleared area
an underlay reveals it at half strength (`CUTOUT_REVEAL_OPACITY`). App text
overlaying it: the composer's input text and placeholder (on the composer
plate) and the device/project and checkout/ref selector labels. Attempt 1
showed the composer's ordinary plate is too thin over artwork (1.40:1). With
artwork set, the pill now paints `artwork_composer_plate(strength)` — its
usual plate thickened only until text and placeholder clear 4.6:1 over both
all-white and all-black artwork at that strength — and the selector rows sit
on the opaque `artwork_selector_plate` (the input plate over the chrome, no
image through it). The audit measures those roles over all-white and all-black
artwork — every image lies between the extremes per channel, so these bound
the possible luminance — at full hero strength for the selectors (the
feather's worst case) and half strength under the composer.

Status is never conveyed by color alone: sidebar and Related rows pair each
status dot with a word (Working, Approval, Input, Failed, Queued); workbench
tabs now use a distinct glyph per status (spinner, shield, bubble, triangle)
plus an accessible name ("…, needs approval").

## Findings and fixes

Before-state ratios below were computed from the Luna built-in palette
values with the app's own compositing formulas (opaque surface, and frosted
over the adverse desktop with the contrast-checked glass coverage). They
document why each fix exists; the run's matrix is the acceptance evidence.

| # | Finding | Before (Luna) | Fix |
| --- | --- | --- | --- |
| F1 | Secondary labels faded far below the text floor in dark appearance: sidebar/Related project & branch lines (`text_muted × 0.5`), captions at 0.35–0.45 ("You", reset times, device tag, empty-state hints). | Dark subline 2.87:1 opaque, 2.29:1 frosted; 0.4 labels 2.25:1 | `readable_muted/faint/tone` now keep the requested quietness but are floored at 4.5:1 on every surface they sit on (`Theme::legible`: raise opacity, then move its lightness, keeping its hue — F25). Raw `text_muted.opacity(…)` label sites moved onto the helpers. |
| F2 | Frost coverage only guaranteed muted text 3:1 over a bright desktop; `text_faint` fell under 3:1. | Dark muted 4.61:1, faint 2.94:1 frosted | Foreground-only roles (`text_muted`, `text_dim`, `text_faint`, muted status tones) get a derived composited floor at palette build for every theme, curated included. Stored themes/preferences are untouched. |
| F3 | Popup hints (`text × 0.48` on floating cards) below the floor. | Light 2.86:1, dark 4.29:1 | Popup muted/faint roles are floored on the floating card itself, keeping the primary > muted > hint ladder. |
| F4 | Focused-pane outline (accent × 0.45), unfocused tab indicator (muted × 0.45) and drop-preview outline (accent × 0.7) failed non-text contrast. | Ring 2.04:1 dark, 1.98:1 light; indicator 2.52:1 / 2.07:1 | All use `readable_icon` (3:1 floor). |
| F5 | Workbench tab statuses differed by dot color only. | — | Distinct glyphs per status at the non-text floor; accessible names include the status. |
| F6 | Status words and the Run failed strip painted in raw danger/warning tones. | Theme-dependent | `readable_tone(tone, 1.0)`: the hue kept, floored at 4.5:1. |
| F7 | Imported and some built-in syntax colors are authored for opaque editors, not the composited code plate. | Measured by the run | Syntax roles derive a floor on the code plate, keeping each hue and moving only its lightness as far as needed. Inline code (`code_text`) is floored on its wash. |
| F8 | Button labels on solid/accent/destructive fills relied on authored on-colors. | Measured by the run | `on_solid`/`on_accent` fall back to white or black, whichever reads better, only when the authored color fails 4.5:1. |
| F9 | Focus edge (`border_strong`) on composited chrome/canvas. | Measured by the run | Floored at 3:1. |
| F10 | Decorative ink was mixed with labels in the same helper, so fixing labels would have darkened separators and illustrations. | — | `decorative_muted` for separators ("·", "/", "›"), illustrations and disabled glyphs: a documented WCAG exemption, never for information. |
| F11 | Terminal text in theme colors had no floor; low-contrast ANSI slots (bright black on dark, yellow/white on light), dim and selected text could fall below 4.5:1. | Measured by the run (authored ratios are recorded as informational rows) | Render-time minimum contrast for theme-controlled pairs, with the conceal/graphics/program-color guards. |
| F12 | New-thread selector labels sat directly on user artwork. | Worst case is white artwork under dark text or black under light text | Selector rows get the composer's plate when artwork is set (superseded by F21). |
| F13 | Primary text failed hovered/selected chrome and the tab wash on some frosted themes (attempt 1, e.g. tokyo-night 4.31:1 hovered). The frost search only checked plain chrome. | Attempt 1 | The frost coverage search now requires `text` at 4.6:1 on chrome, hover, selected, canvas, tab wash and code plate; memoized per palette. |
| F14 | `legible_on` fell back to `theme.text` even where `text` itself failed (white on ANSI bright yellow). | Attempt 1 | Contextual fallback: `text` if it reads, else white or black, whichever reads better. |
| F15 | Badges (Main, Open, kind) floored on reading surfaces, not on their own tint. | Main badge 218 failures | `readable_on(tone, wash, target)`: floored on the wash over every reading surface; glyphs at the non-text target. |
| F16 | Composer selected text failed (251) — the selection wash was too strong, and the audit measured it on the wrong plate. | Attempt 1 | Selection thinned (≥ 12%) until text reads on it over the input plate, the input over the canvas and the frosted pill tint; the audit now uses the pill as painted. |
| F17 | Terminal selected text (189) and the cursor (504). | Attempt 1 | Selection thinned on the terminal panel; cursor floored at 3:1; cursor glyph redrawn above the block (see Terminal). |
| F18 | Destructive labels shared `on_accent` (66). | Attempt 1 | `on_danger()` derived for `danger_strong` alone; `on_accent` derived for `accent_strong` alone. |
| F19 | Focus ring/indicator used raw accent (vscode-dark-plus frosted `#007acc` on `#464646`). | Attempt 1 | `focus_ring()`: the accent floored at 3.1:1 on every reading surface (and, after F26, on the tab chip fills); used by dialog buttons, tab focus and the focused-group underline. |
| F20 | Diff rows: markers, line numbers, accent bars and ±N counts painted raw `diff_add/del`; plain code at `text × 0.92` was never floored; syntax was floored on the code plate but not the row wash. | Attempt 1 (model error hid them) | `diff_row_paint`/`diff_plain_text`/`diff_hunk_text` floor each on the row's 5.5% wash; syntax roles are floored on both diff washes too, re-derived when the diff scheme changes. |
| F21 | Artwork plates (1.40:1). | Attempt 1 | `artwork_composer_plate`, `artwork_selector_plate` (see New-thread artwork). |
| F22 | Raw status/accent copy at real call sites (Appearance, Files, History, Settings…). | Codex review | Inventory above: 59 sites classified and floored. |
| F23 | Settings cards and dialogs were not reading surfaces, so helper floors did not cover copy on them. | Codex review | `reading_surfaces` adds the settings card and the dialog over its scrim. |
| F24 | Cached `readable_on` would cost a reading-surface search per badge per frame. | — | Memoized with `legible` in one per-palette table keyed by (color, backing, floor). |
| F25 | **Hue shift.** The floor moved a tone toward `text` with `mix()`, which interpolates HSL hue linearly (written for nearby spinner hues). Amber `#a16207` (35.5°) became green `#467915` (90.6°) as the Main badge glyph on Luna Light. | Attempt 2 `hue-review.json` | `legible_on` now keeps hue and saturation: it raises opacity, then walks lightness (toward the side `text` reads on, then the other), and only where no lightness of that hue reads falls back to neutral white/black. `mix()` itself is unchanged for its animation callers. |
| F26 | Badges, the focus ring and selected-row checks were floored on their own wash over the reading surfaces, but painted over a **second** layer: the tab chip fill (resting / hovered 5% / active 9%) or the selected menu row (`card_selected_bg`, hovered). Main badge glyph 76, Main badge on the active tab 116, tab focus ring 38, selected-row checkmark 10. | Attempt 2 | `readable_on_stacks` floors on each layer stack over every reading surface; `readable_on_tab_badge` (Main badge text and glyph), `focus_ring` (all three chip fills), `selected_row_icon` (Appearance menus' checks) and the picker hand-off check (resting and hovered rows) use the stacks they are painted on. One more raw tone found and floored: the picker's warning check (`pickers.rs`). |
| F27 | Raw-tone inventory missed multi-line `if … { theme.warning }` forms. | Attempt 2 review | Swept; the picker setting-choice check now uses `readable_icon`. |

Not changed (by design): the primary `text` color itself (the frost coverage
search raises the glass instead, F13), accent and status fills (they are
fills, and the labels on them are floored instead), and every stored setting.

### Performance and caching

The floors are memoized per thread, keyed by a hash of the raw palette inputs
every reading surface derives from (variant, appearance, surface treatment,
text, muted, background, shell, raised, card, dialog, overlay, input, hover
and selected roles), so the key itself costs a hash, not a tint search. A
hot-reloaded theme or a hardened foreground changes the key. The audit's
fresh-surface re-check and its timing row cover both (failure case 6 and 7).

## Results

### Attempt 1 (Codex run, before F13–F24)

810 combinations; **4,210 failing pair instances**; the process exited 0
despite writing `failure.txt` (fixed: the audit now exits 1 from inside the
app callback after writing every artifact — the headless platform can end
the process on quit without returning from `run`). Timing passed
(`readable_muted` 2.85 µs/call; a warm 160×50 terminal frame 3.56 ms).

Failures by group, and what each turned out to be:

| Group | Instances | Cause |
| --- | --- | --- |
| Diff added / deleted line text | 810 each | **Audit model error**: measured on solid `diff_add/del`; the painter uses a 5.5% wash. Corrected; the real painter then exposed F20. |
| Cursor block | 504 | Real: F17. |
| Composer selected text | 251 | Real + model (wrong plate): F16. |
| Main badge | 218 | Real: F15. |
| Terminal selected text | 189 | Real: F17. |
| Artwork composer/selector | 63–189 | Real: F21. |
| Cached-floor re-check | 176 | Real: primary text failing hovered frosted chrome (F13). |
| Hovered / selected navigation | 63 / 54 | Real: F13. |
| Destructive label | 66 | Real: F18. |
| Active tab title | 45 | Real (ayu-light frosted): F13. |
| Inline code | 41 | Real: `text` fallback failing (F13/F14). |
| Hunk header | 27 | **Model error** (painter uses `text_faint`) plus real: now `diff_hunk_text`. |
| Accent button label | 18 | Real: F18. |
| Focus ring / focused indicator | — | Real: F19. |
| Syntax roles | 9 each | Real (ayu-light frosted code plate): F13. |

### Attempt 2 (Codex run, after F13–F24)

810 combinations; the audit correctly exited 1; **240 failing instances in
four element types**, all layered-background cases (F26):

| Element | Instances | Worst |
| --- | --- | --- |
| Main badge on the active tab | 116 | ayu-light, Cyan: `#39617a` on `#c3d0d4`, 4.20:1 |
| Main badge glyph | 76 | catppuccin-latte, Green: `#1a7f4f` on `#b6c5c2`, 2.83:1 |
| Tab keyboard focus ring | 38 | rose-pine-dawn: `#8470a2` on `#d7cfc8`, 2.84:1 |
| Selected-row checkmark | 10 | nord, Luna: `#9592f0` on `#515766`, 2.63:1 |

Hue review (Codex, `appearance-attempt-2/hue-review.json`): Luna Light Amber
`#a16207` (35.45°) painted as `#467915` (90.6°) — F25.

The audit now measures each layered case on every layer combination it is
painted over (resting/hovered/active chips; resting/hovered selected rows),
and adds failure case 15: each derived accent, status, diff and terminal
ANSI tone must stay within 15° of its source hue as painted, on both
desktops, for every combination (`contrast.md` → Hue preservation table;
a white/black neutral fallback has no hue and is not counted as a move).

### Attempt 3 (Codex run, final source, after F25–F27)

**PASS**: exit 0, `appearance-audit.pass` written, `failure.txt` absent;
810 combinations; **0 failures**; 21,375 recorded pairs (216 element rows in
`contrast.md`: 164 text, 32 non-text, 3 exempt, 17 informational). Every
accent, surface and diff-scheme combination was checked at the strict
floors; the minima below are the recorded (default-accent) rows.

| Measure | Result | Where |
| --- | --- | --- |
| Lowest text pair | 4.5827:1 (≥ 4.5) | Terminal palette matrix, selected: default foreground on ANSI 6 cyan — winter-light Frosted, `#ffffff` on `#008290` |
| Lowest non-text pair | 3.1018:1 (≥ 3.0) | Terminal cursor block on the panel — rose-pine-moon, `#6f6d82` on `#232136` |
| Terminal palette matrix (289 pairs each) | plain 4.59, selected 4.58, dim 4.59, glyph under the cursor 4.60 | vscode-dark-plus (default fg on ANSI 5 magenta); winter-light Frosted |
| Main badge on an active tab / glyph | 4.95:1 / 3.39:1 | winter-light; rose-pine-dawn Frosted |
| Tab keyboard focus ring (active / hovered chip) | 3.37:1 / 3.65:1 | winter-light Frosted |
| Selected-row checkmark (resting / hovered) | 3.96:1 / 3.48:1 | nord |
| Artwork: composer text / placeholder / selector label | 4.62 / 4.61 / 5.06:1 | worst of white and black artwork |
| Hue preservation | largest move 0.00006° across all 40 checks (0.0° to one decimal) | `contrast.json` rows `family: "hue preservation"`; `contrast.md` → Hue preservation |
| Exempt (decorative) | 1.27–1.89:1, reported, not required | archive illustration, disabled glyph, breadcrumb separator |
| Informational (authored ANSI slots, program true color) | as low as 1.0:1, reported, not required | e.g. authored ANSI 0 on its own background |
| Timing | `readable_muted` 2.870 µs/call; 160×50 terminal frame 3.452 ms warm | |

Artifacts: `target/qa/super-chat-workbench/appearance-attempt-3/`
(`contrast.json`, `contrast.md`, `appearance-audit.pass`).

The matrix and its timing figures (`readable_muted` per call, terminal
frame) are attempt 3's. The matrix was not rerun after the later
layout-only changes, so no newer timing is quoted here.

### Rendered pixels (workbench fixture run 16, final source)

**PASS**, exit 0. For each of eight theme/surface combinations, step w28
renders the workbench frame, samples three surface pixels from the same
frame it saves as `w28-theme-<variant>-<surface>.png` — the canvas (5 px
inside its left edge, mid-height), the left chrome near the sidebar's
bottom, and the right chrome near the window's bottom-right corner — and
composites each sampled pixel over both desktop extremes (black and white;
the headless window keeps its glass alpha). Three foregrounds are measured
against each: primary `text` and secondary `readable_muted(0.5)` against
**4.5:1**, and the focus edge `border_strong` against **3.0:1**. 8 × 3
surfaces × 2 desktops × 3 roles = 144 pairs; every one met its floor.
Text and focus-edge results are reported separately below: the focus edge
is non-text, so its 3:1 minimum is not comparable with the text minima.
Run 16 (`ui-attempt-16/readback.json` → `renderedContrast`) reproduced run
11's figures exactly. The source changes between the two runs (tab width
cap, frame-row layout, scrollable canvas, focus routing, gesture handling)
are layout and input changes; none edits the theme or its color
derivation, which matches the identical figures.

| Requirement | Role | Lowest rendered ratio | Where |
| --- | --- | --- | --- |
| Text ≥ 4.5:1 | primary text | **4.96:1** (canvas); 5.35:1 on either chrome | catppuccin-latte Frosted, over a black desktop |
| Text ≥ 4.5:1 | secondary text (`readable_muted(0.5)`) | **4.68:1** (canvas); 5.05:1 on either chrome | catppuccin-latte Frosted, over a black desktop |
| Non-text ≥ 3.0:1 | focus edge (`border_strong`) | **3.11:1** (canvas); 3.16:1 on either chrome | catppuccin-latte Frosted, over a black desktop |

Per combination, the lowest of its six samples (three surfaces × two
desktops):

| Variant | Surface | Text | Secondary | Focus edge |
| --- | --- | --- | --- | --- |
| luna-dark | Theme default | 9.15 | 6.80 | 4.21 |
| luna-light | Theme default | 9.82 | 5.27 | 3.13 |
| luna-dark | Opaque | 15.88 | 6.88 | 4.13 |
| github-light | Theme default | 13.70 | 5.98 | 3.32 |
| dracula | Frosted | 8.46 | 6.88 | 4.37 |
| gruvbox-light | Theme default | 8.56 | 6.14 | 4.14 |
| catppuccin-latte | Frosted | 4.96 | 4.68 | 3.11 |
| tokyo-night | Theme default | 10.83 | 6.07 | 4.15 |

The tightest combination is a light, frosted theme over a black desktop,
the adverse case the matrix also bounds (its analytic minima are in the
attempt-3 table). The rendered and analytic results agree: both pass, and
neither is quoted as the other.

**Captures** (`ui-attempt-16/`, visual evidence, not measurements):
w21–w26 the frame in every sidebar state, mid-reflow with the animation
clock pinned halfway through the right sidebar's transition
(`w25-frame-reflowing.png`: 100 ms into the transition the right sidebar
read 287.6 px of its 420 px target, and the frame inset check passed at
that same instant), its reduced-motion equivalent
(`w25b-frame-reduced-motion.png`, the width at its target in the first
frame) and after a sidebar resize; w27 the Related chats, Working tree and
Files top rows in dark and light; w28 the eight themes above. Narrow
windows: `w15b-footer-1220.png` / `w15b-footer-980.png` (both sidebars
open, a long focused title; both Git actions inside the frame row,
labelled at 1220 px and icon-only at 980 px), `w15b-review-1220.png` /
`w15b-review-980.png` (after a real click on Review changes), and
`w15c-scrolled-canvas.png` / `w15c-revealed-focused-group.png` (the saved
split kept at its 320 × 240 group minimum in a scrolling canvas instead of
squeezed). Earlier steps' captures (w01–w20b) show the workbench states
those steps assert (tabs, splits, Main badge, reveal, side histories). The
"Checking delivery" text visible in the narrow-window capture
(`w15-narrow.png`) is the Mock harness's uncorrelated run receipt, left as
the product shows it; native QA verifies real provider delivery.

The narrow-window captures are layout evidence for readability (no
squeezed groups, no overlapping composer controls, actions not under the
sidebar); their geometry is asserted by the fixture (w15b/w15c), but no
additional contrast ratio is measured on them beyond the eight-theme
sample above.

## Uncovered limits

- **Scope of the PASS:** the matrix covers the 30 built-in variants under
  every accent, surface preference and diff scheme, measured analytically
  on composited colors through the production derivation. The rendered
  check covers eight theme/surface combinations at three sampled surface
  points each (Results), on a headless render with the two desktop
  extremes standing in for the real backdrop. Neither is a rendered-pixel
  census of every screen or of every glyph's own pixels.
- **Custom (user) themes** run the same derivation at install but are not
  enumerated; no imported or edited palette was measured.
- **Hue check:** it skips colors too grey for hue to mean anything (sRGB
  chroma < 0.06) and the neutral white/black fallback; it does not report
  how many checks were skipped. Syntax roles are floored hue-preservingly but
  are not in the hue check (their authored source is not kept).
- New-thread artwork: the extremes bound the image's luminance, but the
  feather's exact coverage at each selector pixel is not modeled; the audit
  assumes the worst case (full hero strength). Text elsewhere on the empty
  canvas (none today) would need the same treatment.
- **Terminal, program-controlled colors:** 256-color (16–255) and 24-bit
  true-color pairs paint exactly as the program asks and are not floored by
  design; their contrast is the program's (a sample is recorded as
  informational, 1.0:1). Authored ANSI slots are reported as informational
  too; only their painted (floored) values are required. The cursor block's
  3:1 is measured on the default panel; over an ANSI background or a
  selection its glyph is floored, the block is not. No terminal screenshot
  is in these runs: opening a terminal starts a real PTY; the matrix
  measures the renderer's painted colors directly.
- Settings visuals come from the existing `appearance-fixture` captures; this
  audit adds no new settings screenshots.
- **Platform:** macOS Reduce Transparency and Increase Contrast are not
  read: an explicitly disclosed gap. Surface → Opaque is the in-app
  equivalent. Real desktop backdrops are unknown; the audit bounds them by
  white and black.
- Large-text relaxation is not used; 4.5:1 applies to all text.
- Rendered-pixel checks sample three surface points in eight themes, with
  the theme's own foreground colors measured against them (not the
  anti-aliased glyph pixels); the matrix covers the rest analytically from
  the same compositing.
- **Not a conformance claim.** These results show the measured contrast
  pairs meet WCAG 2.2 AA 1.4.3 and 1.4.11 thresholds for the scope above.
  They do not establish WCAG 2.2 conformance, which also depends on keyboard
  access, focus order, text resizing, non-color cues, motion and
  assistive-technology semantics, none of which this audit measures.
