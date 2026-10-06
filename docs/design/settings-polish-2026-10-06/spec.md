# Settings polish — design spec (2026-10-06)

The user's brief: Settings looks bad. Things are squished together and hard
to read, titles don't read as titles, sections are hard to tell apart,
nothing is aligned, and buttons are scattered. The Providers page is the
worst. Appearance settings (Interface size, fonts, colours) must drive
everything. Every page must look right at every Interface size and window
width: nothing pushed around or wrapped oddly. The bar is the rest of the
app after the UI polish audit (`docs/design/ui-polish-audit-2026-10-06`),
plus macOS System Settings best practice.

This spec is the contract for every agent working on Settings. The shared
primitives live in `crates/ui/src/settings/widgets.rs` and
`crates/ui/src/button.rs`; read both before editing a page.

## 1. Page frame

```
page_column()                      max 720 pt, 32 pt sides, 28 pt top, 72 pt bottom (all rems)
  page_title_bar(title, count, actions)   title left · page actions right, on one center line
  page_subtitle(copy)              optional, secondary body text, ≤ 600 pt measure
  section …                        SECTION_GAP (24 pt) between sections and after the header
```

- **Page actions** (Refresh, a device picker, "Add…") go in the title bar,
  at the right, as kit controls: `button(…, Secondary, Md)` with a leading
  `glyph`, or `icon_button(…, Md)` with a tooltip. A pop-up such as the
  device picker uses `select` or `select_frame`.
- A page-level action is never a bare word floating near the title.

## 2. Sections

A section is **one card that contains its own title**:

```
section_card(theme)
  section_header(title, description?, actions)   title3 15/20 semibold · footnote description · actions at right
  card_row(theme, false) …                        a hairline separates the header and each row
section_footer(copy)                              optional fine print under the card
```

- **Never** leave a section title floating outside its card. The old
  pattern, `field_label("Composer")` above a `section_card`, is replaced by
  a `section_header` inside the card.
- A section that is a single control row may skip the header; its row title
  carries the meaning.
- **Section actions** (Add account, Reset, Reload) go in the header's actions
  slot, at the right of the title. They do not go below the content or on a
  row of their own.
- Group related settings. One card per topic, not one card per setting, and
  not one giant card for the whole page.

## 3. Rows

```
card_row(theme, first)           wraps · ≥ 48 pt · 12/16 pt padding · 12 pt gaps
  [row_tile(icon)]               optional 28 pt identity tile
  row_text(title, description?)  title 13/18 medium (wraps) · description 12/16 secondary (wraps)
  row_controls().child(…)        trailing, right-aligned, vertically centred
  [row_block().child(…)]         full-width controls under the text (chip groups, lists, editors)
```

- **Controls sit in one place: the trailing column, right-aligned.** One
  primary control per row (switch, select, segmented, button). Secondary
  row actions sit beside it, to its left.
- `row_text` holds a minimum width (`ROW_TEXT_MIN`). A row that can't fit
  its controls beside the text wraps them **under** the text. This is the
  responsive behaviour; never let text squeeze to one word per line.
- Titles and descriptions **wrap; they never truncate.** Truncation is only
  for user data (an email, a path) and must have a tooltip or a wider host.
- List rows (accounts, devices, projects, archived chats) use the same
  anatomy: identity (tile or avatar) · text · trailing status badges and
  actions.

## 4. Typography: only the interface scale

| Role | Style |
| --- | --- |
| Page title | `page_header`: Title1 20/25 semibold |
| Section title | `section_header`: Title3 15/20 semibold |
| Row title | `row_title`: 13/18 medium |
| Body, descriptions | `row_description` / `page_subtitle`: 12/16, 13/19 secondary |
| Metadata, badges | Caption 11/14 |

- Every font size is `crate::typography::ui_rems(..)` or `TextStyle`.
  **Never** use `px(..)` for text, or GPUI's `.text_xs()`, `.text_sm()` and
  friends.
- Colours come from the theme only: `theme.text`, `readable_muted`,
  `readable_tone`, `fill(FillTier)`, `separator`, `accent`, `danger`. No raw
  `hsla(..)`, `rgb(..)` or `ink(alpha)` washes in pages.

## 5. Controls: only the kit

From `crate::button` and `settings::widgets`:

- `button` (Primary / Secondary / Ghost / Danger), `icon_button`, `select` /
  `select_frame` + `select_chevron`, `chip`, `segmented`, `selection_ring`,
  `widgets::switch_control`, `widgets::ghost_action`.
- **Size:** `Md` in rows and title bars, `Sm` inside dense list rows, `Lg`
  only in dialogs.
- **Choices:**
  - A 2–5 option choice is a `segmented`.
  - An open-ended list (models, projects) is a `select` or a `chip` group.
  - On/off is a switch.
- **Destructive actions** (Disconnect, Delete, Remove) are
  `ButtonKind::Danger` or Secondary with a confirm. They are never the
  visually loudest thing in the row; the solid white "Disconnect" pill is
  wrong.
- No bare text links as buttons, and no hand-rolled `div` buttons. If the kit
  lacks something, ask the orchestrator; don't fork it.

## 6. Scaling: Appearance drives everything

- Interface size runs 12–20 px (default 16). The window's rem is the
  Interface size, so **every dimension that sits next to text is a rem**:
  control heights, row heights, padding, gaps, icon and tile sizes, column
  widths, segmented widths. Use `ui_rems(points_at_16px)`.
- Only these stay in `px`: 1 px hairlines, corner radii, shadows, and focus
  ring widths.
- No fixed `h(..)` on anything that contains text; use `min_h` and let it
  grow.
- **Verify every page at:**
  - Interface size **12, 16 and 20**;
  - window widths about **900 and 1400** (content column about 560 and 720
    pt), with the sidebar at its default width;
  - **dark and light**.
- At each combination, check that nothing overlaps, clips, wraps to one
  word per line or pushes a control off the column.

## 7. States

- **Loading:** `popover::skeleton_rows` in place of the rows, not a
  centred spinner on an empty page.
- **Empty:** one row with secondary text and, when there is an obvious next
  step, a kit button for it.
- **Errors:** `widgets::error_strip` / `warning_strip`, inside the section
  they belong to.

## 8. Page direction

**Providers** (`accounts.rs`, `provider_defaults.rs`): the worst page, so
fix it first.
- **Title bar:** "Providers" with the device picker (`select_frame`, monitor
  glyph, the device name and its online dot) and a Refresh `icon_button`
  (REFRESH glyph, tooltip "Refresh accounts and usage") at the right. No
  floating "Refresh" word.
- **A "New chats" section** with one row: "Default provider", a
  `segmented` over the enabled providers.
- **One section card per provider:**
  - **Header:** brand mark · name · version inline beside the name as a
    caption (`CLI 0.160.0`). When an update is available, an accent caption
    `2.1.291 available` sits next to it and an `Update` button (Secondary,
    Sm, arrow-down glyph) goes in the header actions. The header actions
    are, in order: Update, then Add account / Sign in another, then the
    enable switch, last.
  - **Account rows:** avatar · email · plan and "Active" badges · usage
    meter with its label · trailing Disconnect (Secondary or Danger, Sm).
  - **Defaults rows**, *inside the provider's card* (no separate
    "Defaults for new chats" card): Model (`select`), Reasoning
    (`segmented` or `select` when it has more than 5 levels), Speed
    (`segmented`), and a "Use provider defaults" ghost action in the
    defaults group.
  - A disabled provider collapses to its header, muted, with the switch off.
- **Advanced provider options:** a proper disclosure row (chevron, title,
  description) in its own section, not a full-width grey bar.

**Projects** (`project_defaults.rs`, `project_commands.rs`):
- **Title bar:** the project picker as a `select`.
- **Sections:** Defaults (provider and model, permission, checkout) as rows
  with `select`, `segmented` or `chip` controls; Commands as list rows with
  trailing kit actions.

**General**, **Notifications**, **Files**, **Shortcuts**, **Appshots**,
**Devices**, **Backups**, **Archived**, **Automations**, **Usage** and
**Updates**: move titles into the cards and rows onto the row anatomy,
rebuild controls on the kit, and apply the scaling and state rules above.

**Appearance**: it already uses the kit. Bring it onto sections with inner
headers, and verify that every Appearance setting really drives the app:
- Interface size scales all interface text and controls, including the
  Settings nav and page chrome.
- Fonts apply to every surface they claim.
- The code and terminal sizes apply to every code surface.
- Theme and accent colours have no hard-coded exceptions.

## 9. Ownership and workflow (agents)

Each agent owns only the files listed in its brief. **Don't edit these
shared foundations**; ask the orchestrator for changes instead:
- `crates/ui/src/settings/widgets.rs`
- `crates/ui/src/button.rs`
- `crates/ui/src/interaction.rs`

Compile-check with `scripts/cargo.sh check --locked -p luna-ui` in your
worktree:
- At most a few times: after the main edit and before pushing.
- Exit 75 means the shared build slot is busy. Wait about three minutes
  and retry; never bypass the guard.
- Don't run the full test suite or `run-macos-dev.sh`. The orchestrator
  integrates, builds, runs the fixtures and reviews.

Commit on your branch and push it as `settings-polish/<area>`; don't merge
into `main`. End every commit message with the line
`Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
