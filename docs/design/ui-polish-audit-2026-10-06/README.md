# UI polish audit — 2026-10-06

Requested 2026-10-06; source baseline `590cf4b6`. Audit only: no product code is
changed by this task.

## Goal

A completely unified, premium UI: everything smooth, properly sized, aligned,
readable, with proper icons and motion. The bar is an Apple product (iPhone,
MacBook, macOS system apps). The chat timeline — tool calls, expandable
disclosures, streaming — gets extra scrutiny.

For every finding we record what is wrong, a mock of the issue, and a mock of
the proposed fix.

## Layout of this folder

| Path | Contents |
| --- | --- |
| `README.md` | This index, conventions and the consolidated summary |
| `findings/<area>.md` | One file per audit area, every finding in the format below |
| `mocks/<AREA>/<ID>-before.svg` / `<ID>-after.svg` | Self-contained SVG mocks of the issue and the proposed fix |
| `shots/<area>/` | Real renders captured from the production views (headless fixtures or the LUNA Dev app), cropped to the issue |
| `report.html` / `report.pdf` | Assembled visual report: every finding with its before/after mocks side by side, filterable by area and severity |
| `build_report.py` | Rebuilds the report from the findings files (command at the end of the summary) |

## Areas and ID prefixes

| Prefix | Area | Findings file |
| --- | --- | --- |
| `TL` | Chat timeline: messages, tool calls, disclosures, streaming, markdown/code in chat | `findings/timeline.md` |
| `CP` | Composer, composer dock, queue, pickers, popovers, menus | `findings/composer.md` |
| `SH` | App shell: sidebar/rail, title bar, tabs, workbench panes, empty/new-thread states, command palette, history, activity | `findings/shell.md` |
| `DS` | Design-system foundations: theme tokens, typography scale, spacing/radius, icon set, motion curves; Settings as the consumer | `findings/design-system.md` |
| `SS` | Secondary surfaces: files/editor, changes/diff/PR, terminal, browser, image viewer, notices/toasts, usage, automations | `findings/surfaces.md` |

## Finding format

```markdown
### TL-01 — Short title
- **Severity:** P0 (broken/unreadable) · P1 (clearly off-brand, visible daily) · P2 (polish) · P3 (nit)
- **Category:** motion · alignment · sizing · spacing · typography · color/contrast · iconography · consistency · interaction · states
- **Where:** `crates/ui/src/file.rs:123` (+ fixture name / screenshot path)
- **What's wrong:** concrete description with measured values (px, pt, ms, ratios)
- **Why it hurts the premium feel:** one or two sentences
- **Proposed fix:** concrete values/tokens/approach, naming the shared helper to use or add
- **Before:** `mocks/TL/TL-01-before.svg` (and/or `shots/timeline/…png`)
- **After:** `mocks/TL/TL-01-after.svg`
- **Effort:** S · M · L
```

## Mock conventions

- SVG, self-contained (no external fonts/images), ~720 px wide, drawn with the
  real Luna theme colors, font sizes, radii and spacing read from the code.
- "Before" reproduces the defect faithfully and marks it with red callouts.
  "After" shows the proposed fix with green callouts naming the values.
- Real renders (`shots/`) are preferred evidence for the "before" state when a
  fixture can show the surface.

## Summary

**117 findings** across the five areas: 1 P0, 46 P1, 59 P2, 11 P3.

The color system is solid: the contrast audit passes all 810 combinations of
theme, accent and surface. Most other gaps share one cause. Sizes, spacing,
radii, motion and controls are typed in place at each call site instead of
coming from shared tokens and components. As a result, the same control is
drawn several different ways, and most state changes snap.

### What keeps it from feeling premium

1. **No shared control kit.**
   - 9 button and picker recipes (DS-02), six control styles in the composer row alone (CP-07), and 8 control sizes in the chrome (SH-10).
   - Three tab recipes (SH-06, SS-07).
   - At least 5 tooltip implementations (CP-12, SH-21, DS-20).
   - 7 focus treatments (DS-17, SH-19, SS-16) and 25 hover fills (DS-05, SH-12).
   - Six empty-state recipes (SS-12, SH-24) and no shared toast (SS-08).
2. **Motion snaps or moves the wrong way.**
   - Tool disclosures are a 140 ms height wipe, and the chevron swaps glyphs instead of rotating (TL-01, TL-02).
   - The command palette, image lightbox, Send/Stop button, switches and segmented controls all appear in a single frame (SH-04, SS-09, CP-10, DS-06).
   - Menus fade in from 30% opacity and move toward their anchor instead of away from it (CP-08).
   - Switching chats blanks the transcript and then pops the new one in (SH-17).
   - The code uses 16 durations and 6 curves with no spring, and never reads macOS Reduce Motion (DS-15, DS-04).
3. **The tonal hierarchy collapses.**
   - Muted and faint text render as the same grey in the timeline (TL-05).
   - The composer placeholder is almost as bright as typed text (CP-03).
   - A selected sidebar row looks the same as a hovered one (SH-02).
   - In light mode, disabled Back/Forward look enabled (SH-08).
   - The working indicator's verb and timer are the same grey (TL-24).
4. **Type and geometry drift.**
   - 17 font sizes, most using GPUI's 1.618 default line height (DS-01), and 94 sites below 11 pt (DS-18).
   - 17 corner radii, with nested corners that don't follow their container (DS-10).
   - Half the spacing values are off the 4-pt grid (DS-11).
   - Header bands of the two tab systems sit 6 px apart (SH-06, SS-06), and content insets vary between 8, 12, 16 and 20 pt (SS-14).
5. **Readability breakers.**
   - Transcript text shows through the composer notice (SH-01, the only P0).
   - Thinking text is hard-wrapped at 96 columns and then clipped (TL-03).
   - Diff add/remove tints are about 1.1:1, with no word-level highlight (SS-02).
   - 13 scroll regions have no scrollbar (SS-03).
6. **Icons.**
   - About 47 of 112 glyphs are hand-drawn or ported instead of Solar Linear, and some draw 25–60% heavier at 16 px (DS-09).
   - Icons come in 17 sizes with no size tokens (DS-12).

### Working indicator (requested during the audit)

TL-24 covers the "Marinating… 13s" row:
- **Glyph:** about 2.6 of its 9 dots are visible at any moment, and dots at rest are 1.1:1 against the canvas.
- **Motion:** it ticks at 15 Hz, so it flickers instead of moving as a wave.
- **Verbs:** a whimsical verb hard-cuts every 7 s.
- **Timer:** the digits are proportional, so the timer jumps sideways by up to about 45 px.

The finding mocks three directions. The recommendation is option C with
option A's motion:
- **Text:** the status names the actual work, such as "Running cargo test · 13s".
- **Motion:** a breathing accent orb, plus a soft shimmer across the text.
- **Timer:** steady-width digits in a quieter tone.
- **Stop:** a quiet Stop action on hover.

Playful verbs become an opt-in setting.

### Recommended order

1. **Foundations.** Build these once, as design-system tokens; values are under "Proposed unified tokens" in `findings/design-system.md`:
   - `TextStyle`, a type scale with paired line heights
   - `space`, `radius` and `radius::inner`
   - `ControlSize`
   - label, fill and separator roles
   - motion tokens, including a disclosure spring, plus observing the system Reduce Motion setting
   - `IconSize`
2. **Shared components on those tokens:**
   - Button, IconButton and Chip
   - Tooltip
   - an anchored Popover/Menu with enter and exit animations
   - a focus-visible ring
   - Tabs, Toast and EmptyState
   - Disclosure and Scrollbar
3. **Daily surfaces next:**
   - the working indicator (TL-24) and tool disclosures (TL-01, TL-02)
   - the composer controls row and menus (CP-07, CP-01, CP-08)
   - sidebar selection and the tab strip (SH-02, SH-03)
   - the composer notice (SH-01) and the command palette (SH-04)
4. **Secondary surfaces after that:**
   - the diff view (SS-02), toasts (SS-08), the lightbox (SS-09)
   - the file tree (SS-10) and terminal scrolling (SS-05)
   - Settings rows (DS-03, DS-07, DS-08)
   - the Solar icon pass (DS-09)

### Evidence and limits

- **Real renders:** they come from the headless production fixtures, all 40 built at `590cf4b6`.
- **Motion:** the motion mocks are filmstrips built from the code's constants and curves, because no fixture captures frames mid-animation.
- **Code-only areas:** no fixture renders Files, Diff, Terminal or Browser. Those areas were audited from code, with file:line references.
- **Fixtures that didn't complete:**
  - Some fixtures were skipped because they open visible windows.
  - Others need engine artifacts or failed partway through.
  - `composer-parity-fixture` currently fails at a 300 pt pane, which is finding CP-06.

Rebuild the visual report after editing findings:

```sh
agent-python docs/design/ui-polish-audit-2026-10-06/build_report.py --pdf docs/design/ui-polish-audit-2026-10-06/report.pdf
```
