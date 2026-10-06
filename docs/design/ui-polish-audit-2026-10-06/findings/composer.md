# Composer, dock, queue, pickers, popovers and menus (CP)

## Area overview

The composer is well engineered. Its height morph, the docking spring and the
width-aware controls row are all thoughtful, and the main/side-chat parity holds
up. What keeps it from feeling like an Apple product is unity. The one controls
row mixes six control styles: three heights, five hover washes, instant and faded
hovers, and icons of two sizes. The composer area also carries five separate
tooltip implementations, and its menus use three typographic systems and two
highlight models. The motion layer is inconsistent too. Menus fade in from 30%
opacity, move the wrong way when they open upward, and pop in a full-strength blur
slab, while completion popups have no exit at all. Some problems are plainly
visible in real renders. The model and access menus open detached from their
chips. Tooltips cover the control they describe. The empty-composer placeholder
is almost as bright as typed text. Integration rows zig-zag, and a completion
popup slices the new-thread destination row. At a 300pt pane the controls row
overflows, and the shipped composer-parity fixture fails on it.

Evidence was rendered headlessly from production views (Luna Dark unless noted,
2x) with `composer-parity-fixture`, `integrations-picker-fixture`,
`chat-references-fixture`, `queue-image-steer-fixture` and `goal-card-fixture`.
The parity and queue fixtures were also run with `LUNA_OPEN_PICKER=model` and
`=permission` so the popovers render open. Crops are in `shots/composer/`.
Colours quoted as "rendered" were sampled from those PNGs. Sizes come from the
source constants cited.

---

### CP-01 — Model and access menus open detached from their chips
- **Severity:** P1
- **Category:** alignment
- **Where:** `crates/ui/src/pickers.rs:8272` (`attach_overlay_end`), `:8361` (model 304pt), `:8368` (access 340pt), `crates/ui/src/popover.rs:662` (`anchored_menu_above_end`). Shots: `shots/composer/CP-model-popover-detached.png`, `shots/composer/CP-permission-popover-detached.png`
- **What's wrong:** In an established chat both chips sit at the left of the controls row, right after the paperclip, but their menus still use the "right-edge control" anchor (`Anchor::BottomRight` at the chip's right edge). The 304pt model card and the 340pt access card are wider than the space to the chip's left, so `snap_to_window_with_margin(8)` shoves them to the window gutter. In `queue-image-steer-fixture` the model card spans x 9→311pt while its chip ends at 248pt. The card overhangs the composer's leading edge by 98pt, covers the queue row, and no longer lines up with its trigger. The access card behaves the same way (x 8→348pt against a lock chip at 252→284pt).
- **Why it hurts the premium feel:** macOS menus grow from their trigger. A card that lands somewhere unrelated, over the sidebar or the neighbouring pane in a split, looks like a layout bug.
- **Proposed fix:** Anchor composer-row chips at their leading edge with `anchored_menu_above` and an x offset of `-CARD_INSET` (−4pt), so the row text lines up with the chip glyph. Flip to `_end` only when the card would cross the 8pt window gutter. Use the same rule for the new-chat `anchored_menu_below_end` variants.
- **Before:** `mocks/CP/CP-01-before.svg` (+ `shots/composer/CP-model-popover-detached.png`)
- **After:** `mocks/CP/CP-01-after.svg`
- **Effort:** S

### CP-02 — Hover tooltips cover the control they describe
- **Severity:** P1
- **Category:** interaction
- **Where:** `crates/ui/src/pickers.rs:4946` (chip `.tooltip`), `crates/ui/src/composer.rs:12796` (attach), `crates/ui/src/composer/goals.rs:947`, `crates/ui/src/context_usage.rs:125`. Shots: `shots/composer/CP-model-tooltip-covers-chip.png`, `shots/composer/CP-access-tooltip-covers-send.png`
- **What's wrong:** Every composer tooltip is GPUI's cursor-anchored tooltip. With the controls row at the bottom of the window the card opens just below and right of the pointer, so it lands on the row itself. In parity `c02` the model card ("Sonnet 5.5 / Low · 200K · Standard") covers the lower half of the chip label and the access icon. In `c03` the 262pt "Full access" card covers the usage ring and half of the Send button.
- **Why it hurts the premium feel:** The explanation hides the thing being explained, and it hides the primary action. Apple anchors help tags and popovers to the control, not to the pointer.
- **Proposed fix:** Add `popover::anchored_tooltip(trigger_bounds, Placement::Above)`. It centres on the trigger with a 6pt gap and clamps to the window gutter. The composer already positions its own mention tooltip (`MentionTooltipTarget`, `composer.rs:1440`), so this generalises existing code. Show it after 400ms, with no delay while warm (another tooltip shown less than 1s ago), and fade in over 120ms (`FADE_QUICK`).
- **Before:** `mocks/CP/CP-02-before.svg` (+ both shots above)
- **After:** `mocks/CP/CP-02-after.svg`
- **Effort:** M

### CP-03 — Placeholder is almost as bright as typed text (flat tonal hierarchy)
- **Severity:** P1
- **Category:** color/contrast
- **Where:** `crates/ui/src/composer.rs:12314` (placeholder = `theme.text_faint`), `crates/ui/src/theme.rs:1944` (`harden_composited_foregrounds`), compared with `theme.rs:1442` (`for_popup`). Shot: `shots/composer/CP-composer-idle-dark.png`
- **What's wrong:** On the rendered pill (#080808) the tones are:
  - "Do anything…" placeholder: #CCCCCE, 12.5:1
  - typed text: #E8E8EA, 16.4:1
  - model label: #D2D2D3, 13.6:1
  - effort suffix "Low": #C6C6C8, 11.8:1

  All four sit within 1.4× of each other. The authored faint tone is #85858A (~5.4:1), and `theme.rs:3004` says faint is meant to stay around 4.1–4.2:1. But `harden_composited_foregrounds` lifts `text_faint` to 4.5:1 against the *adverse* backdrop, a white desktop behind 80% glass, so in practice it lands at #CCCCCE. Popups avoid this because `for_popup()` rebuilds a translucent ladder (text at 0.64 and 0.48) floored on the card's own surface. The composer gets no equivalent.
- **Why it hurts the premium feel:** An empty composer reads as if it already holds text, and the chip row has no lead. Apple placeholders recede clearly (tertiary label).
- **Proposed fix:** Add a `Theme::for_composer()` (or a dedicated `placeholder` role) that floors faint and muted on the composited composer plate, as `for_popup` does for cards. Targets: placeholder about #85858A (≥4.5:1 on the plate), secondary or effort #A9A9AE (8.6:1), model label at full `text`. Re-run `appearance-audit` (its case 10 covers composer text over artwork).
- **Before:** `mocks/CP/CP-03-before.svg` (+ shot)
- **After:** `mocks/CP/CP-03-after.svg`
- **Effort:** M

### CP-04 — Integration rows: the subtitle lands as a zig-zag trailing column
- **Severity:** P1
- **Category:** alignment
- **Where:** `crates/ui/src/composer.rs:8955–9015` (native mention rows inside `render_file_mention_popup`). Shot: `shots/composer/CP-integration-rows-zigzag.png`
- **What's wrong:** Each native-integration row passes two children to `menu_row`: a title row (wand · name · kind) and a description with `.pl(22)`, clearly meant as line two under the title (22 = 14pt icon + 8pt gap). `menu_row` is `flex_row`, so the description becomes a trailing column. The kind labels then start wherever the description's width allows. In `popup-ready.png` "Apps" and the two "Plugins" labels start at three different x positions, up to 34pt apart, and the intended subtitle indent is lost.
- **Why it hurts the premium feel:** Ragged columns inside a menu are the first thing a designer's eye catches, and here they look broken rather than stylistic.
- **Proposed fix:** Wrap the body in `flex_col`: a 13pt title, then a 12pt muted subtitle, both on the 34pt text column. Move the kind into a right-aligned 16pt badge (reuse `conversation_type_badge`, `chat_references.rs:741`).
- **Before:** `mocks/CP/CP-04-before.svg` (+ shot)
- **After:** `mocks/CP/CP-04-after.svg`
- **Effort:** S

### CP-05 — Completion popup slices the new-thread destination row
- **Severity:** P1
- **Category:** states
- **Where:** `crates/ui/src/composer.rs:13081–13117` (popups attached to `pill_surface`, destination row rendered above it), `crates/ui/src/popover.rs:636` (`full_width_menu_above`, 6pt gap). Shot: `shots/composer/CP-selector-row-sliced.png`
- **What's wrong:** On the new-thread canvas the "This device ⌄ · Select project ⌄" row sits directly above the pill. When an `@`, `#` or `/` popup opens, its card ends 6pt above the pill, right across that row. Only the bottom ~5pt of the glyphs stays visible, blurred, in the gap between the card and the pill. See the integrations fixture at 2x: the text is cut horizontally by the card edge.
- **Why it hurts the premium feel:** Half-glyphs peeking out from under a card look like a rendering glitch at the point the user is typing.
- **Proposed fix:** While any completion token is active, drive the destination row's opacity to 0 over 120ms (`FADE_QUICK`), using the `new_thread_chrome_opacity` plumbing that already exists, and restore it on close. The alternative is to anchor the full-width popup above the destination row. Hiding the row is simpler and keeps the card next to the text it completes.
- **Before:** `mocks/CP/CP-05-before.svg` (+ shot)
- **After:** `mocks/CP/CP-05-after.svg`
- **Effort:** S

### CP-06 — Controls row overflows at ~300pt panes (the parity fixture fails)
- **Severity:** P1
- **Category:** sizing
- **Where:** `crates/ui/src/composer.rs:477–520` (`controls_row_fit`), `:434` (`controls_row_fits`), `:440` (`controls_row_budget`). Evidence: `composer-parity-fixture` exits 1. Its `failure.txt` reads "pane controls row needs 100.0px but has 98.0px" (surface 190pt).
- **What's wrong:** At a 300pt pane the row budget is 98pt (190 − 2 − 24 − 28 − 2 − 8 − 28). The leanest candidate (icon-only model 32 + 4 + access 32 + 4 + usage ring 28) needs 100pt, and `controls_row_fit` returns it anyway as its fallback. `controls_row_fits()` only checks 2×32 + 4 = 68pt, so the compact tray never takes over. The row then squeezes the `min_w_0` model wrapper by 2pt. This is the exact case the fixture was written to catch ("a chip label is cut").
- **Why it hurts the premium feel:** Clipped glyphs at common side-pane widths, and a red E2E gate.
- **Proposed fix:** Add a sixth candidate that hides the usage chip and shows its share in the model tooltip and access card. Make `controls_row_fits()` count every always-shown chip, so the tray engages before any overflow. Re-run the parity fixture at 300, 360 and 760pt.
- **Before:** `mocks/CP/CP-06-before.svg` (+ `shots/composer/CP-min-width-row.png`)
- **After:** `mocks/CP/CP-06-after.svg`
- **Effort:** S

### CP-07 — One controls row, six control styles
- **Severity:** P1
- **Category:** consistency
- **Where:** attach `crates/ui/src/composer.rs:12780` (28 circle, `ink(0.10)`, `hover_blend` fade); model and access `crates/ui/src/pickers.rs:4906–4935` (h32 r8, `element_hover`, fade); Goal `crates/ui/src/composer/goals.rs:924–940` (h32 r8, own wash, instant `.hover`, icon 15); usage `crates/ui/src/context_usage.rs:113–123` (h24 r6, `ink(0.05)`, instant, 11pt); Work `crates/ui/src/composer/work.rs:417–428` (h32 r8, instant, 11pt); Send `composer.rs:12196` (28 disc, instant opacity 0.85)
- **What's wrong:** Side by side the row has heights 24, 28 and 32, radii 6, 8 and 14 (circle), five different hover washes (three of them instant), icons of 15 and 16pt, and labels of 11 and 12pt. The hover plate of the model chip is 32pt tall while its neighbours, the paperclip and Send, are 28pt circles.
- **Why it hurts the premium feel:** This row is the most-used surface in the app. When its controls look like six different components, the app reads as assembled rather than designed.
- **Proposed fix:** Add one `composer_control()` helper. Height 28; capsule radius 14 (an icon-only control is a 28pt circle); no plate at rest; `element_hover` on hover or open via `hover_blend` (`HOVER_FADE` 150ms); a 16% wash when pressed; the CP-15 focus ring; 16pt icons; 12/600 labels with a #A9A9AE secondary. Send stays the only solid disc. `ACTIONS_ROW_HEIGHT` drops from 46 to 42.
- **Before:** `mocks/CP/CP-07-before.svg`
- **After:** `mocks/CP/CP-07-after.svg`
- **Effort:** M

### CP-08 — Popover entrance: wrong direction, opacity pop, blur snaps in
- **Severity:** P1
- **Category:** motion
- **Where:** `crates/ui/src/motion.rs:374` (`MENU_IN` 140ms `EASE`), `:533` (`menu_in`: opacity 0.3→1, top −2→0), `crates/ui/src/popover.rs:410` (`frosted_menu`: blur ramps only on exit), `:446` (`menu_motion`)
- **What's wrong:**
  1. Frame 0 already paints the card at 30% opacity, so it pops rather than fades.
  2. The −2pt travel is the same for every placement. A dropdown moves away from its trigger, but every *upward* menu, which covers all composer pickers and completions, starts 2pt farther from its chip and drops toward it. Two points over 140ms is also below the threshold of perception.
  3. `BackdropBlur` ignores element opacity, so the 16pt frost slab appears at full strength on frame 0 under a 30%-opacity card. The exit already fixes this by scaling blur with progress (the comment at `popover.rs:406` explains why); the entrance does not.
- **Why it hurts the premium feel:** Menus are the most frequent animation in the app. A flash plus a backwards drift makes them feel cheap, where macOS popovers grow out of their anchor.
- **Proposed fix:** `menu_in(origin)`, where origin is Below for `anchored_menu_above*` and Above for `*_below`. Offset 6pt from the anchor side, opacity 0→1, and pass entrance progress to `frosted_menu` so the blur goes 0→16 on the same curve. `MENU_IN` = 180ms `EASE_OUT_QUINT` (an existing token); `MENU_OUT` = 120ms with opacity only. Reduced motion means a fade only.
- **Before:** `mocks/CP/CP-08-before.svg`
- **After:** `mocks/CP/CP-08-after.svg`
- **Effort:** M

### CP-09 — Send looks ready on an empty draft; blocked and queue states are silent
- **Severity:** P1
- **Category:** states
- **Where:** `crates/ui/src/composer.rs:634` (`send_button_mode`), `:9778` (`send_blocked`), `:12196–12245` (`render_send_button`). Shots: `shots/composer/CP-composer-idle-dark.png`, `shots/composer/CP-goal-mode-empty-send.png`, `shots/composer/CP-blocked-send.png`
- **What's wrong:**
  - With an empty draft the Send disc renders as a full-strength #E8E8EA disc, the same pixels as a ready send (parity `c01`, goal `02`). `send_blocked()` never considers an empty draft.
  - The real blocked state (new chat with no project) is a silent 35% fade (#555556) with no tooltip or reason.
  - While a turn runs, Queue mode reuses the Send arrow, so "send now" and "queue for later" look identical.
- **Why it hurts the premium feel:** The primary button is the clearest affordance in the window. It should say whether it will do anything, and what it will do (compare iMessage and Mail).
- **Proposed fix:** For an empty draft or a blocked state, use a recessed disc (white 8%) with a #6E6E73 glyph. It turns solid on the first character with a 120ms fade. Blocked additionally gets an anchored reason tooltip (CP-02), for example "Select a project to start". Queue gets its own glyph and the tooltip "Queue (⏎)".
- **Before:** `mocks/CP/CP-09-before.svg` (+ shots)
- **After:** `mocks/CP/CP-09-after.svg`
- **Effort:** S

### CP-10 — Send ↔ Stop is an instant element swap; hover is an instant opacity step
- **Severity:** P2
- **Category:** motion
- **Where:** `crates/ui/src/composer.rs:12206–12245` (`composer-stop` and `composer-send` are separate element ids), `.hover(|s| s.opacity(0.85))`
- **What's wrong:** When a run starts, the arrow disc is replaced by the stop disc in a single frame. Hover jumps to 85% opacity instantly, while every chip beside it fades over 150ms. There is no pressed state.
- **Why it hurts the premium feel:** The send→stop moment is the app's main state change. A morph tells the user "your message is now running"; a hard cut feels like a re-render.
- **Proposed fix:** Keep one stable `composer-primary` disc and swap only the glyph. Use opacity plus `Transformation::scale` (0.6→1) on the SVG; the transform is already used for chevrons in `transcript.rs:7991` and `shell/spaces.rs:654`. Run it over 180ms `EASE_OUT_QUINT`. Hover goes through `hover_blend` (`HOVER_FADE`) to 88%; pressed is 76% with a 0.96 glyph scale. Reduced motion gets a plain crossfade.
- **Before:** `mocks/CP/CP-10-before.svg`
- **After:** `mocks/CP/CP-10-after.svg`
- **Effort:** S

### CP-11 — Completion popups vanish without an exit; the stash list is a hand-rolled card
- **Severity:** P2
- **Category:** motion
- **Where:** `crates/ui/src/composer.rs:9081`, `:9466`, `crates/ui/src/composer/chat_references.rs:648` (each `full_width_menu_above(…, None)`); `crates/ui/src/composer.rs:11971–12058` (`render_stash_menu`)
- **What's wrong:** The `@`, `#` and `/` popups pass `closing: None`, so on Esc or accept they disappear in one frame, while every picker plays `MENU_OUT`. The prompt-stash list is an inline child of the composer column rather than a deferred menu: no frost, no `menu_in`/`menu_out`, its own row metrics (py 8 · 12pt), an instant `glass_hover` hover and a third focus style (`theme::focus_ring`).
- **Why it hurts the premium feel:** Siblings that animate differently, or not at all, break the sense of one system.
- **Proposed fix:** Hold the token's last rows in a `Popup` and pass `closing_since()` (MENU_OUT 120ms). Rebuild the stash list on `anchored_menu_above` + `popover_card` + `menu_row`.
- **Before:** `mocks/CP/CP-11-before.svg`
- **After:** `mocks/CP/CP-11-after.svg`
- **Effort:** S

### CP-12 — Five tooltip implementations in one composer
- **Severity:** P2
- **Category:** consistency
- **Where:** `crates/ui/src/pickers.rs:571` (`ControlTooltip`), `crates/ui/src/queue.rs:72` (`QueueActionTooltip`), `crates/ui/src/composer.rs:593` (`AppshotActionTooltip`), `:4052` (`MentionPathTooltip`), `crates/ui/src/context_usage.rs:205` (`UsageCard`); unused shared `crates/ui/src/popover.rs:323` (`tooltip_card`)
- **What's wrong:** The five tooltips differ in every attribute:

  | Tooltip | Radius | Padding | Type | Surface and shadow | Delay |
  | --- | --- | --- | --- | --- | --- |
  | `ControlTooltip` | 8 | 10×7 | 12/500 + 11 | opaque, `shadow_md` | 400ms |
  | `QueueActionTooltip` | 6 | 8×5 | 10.5 muted only | frosted, no shadow | 350ms |
  | `AppshotActionTooltip` | 6 | 8×6 | 11 | opaque, `shadow_md` | default |
  | `MentionPathTooltip` | 5 | h24, px 8 | 11 mono | opaque, 150ms fade | 420ms |
  | `UsageCard` | 12 | p12 | 12 + 11 | frosted popover card | default |

  The hardened `border_strong` also renders as a bright #949495 ring on the opaque ones.
- **Why it hurts the premium feel:** Users hover these controls constantly, and four different shapes in one row signal no design system.
- **Proposed fix:** Make `popover::tooltip(title, detail)` the only tooltip: radius 8 · padding 10×6 · max width 260 · opaque `tooltip_surface` · white-12% hairline · shadow 0 4 12 /35% · title 12/600 · detail 11 muted · mono variant for paths only. Combine it with the CP-02 placement and timing; the five types become thin wrappers.
- **Before:** `mocks/CP/CP-12-before.svg`
- **After:** `mocks/CP/CP-12-after.svg`
- **Effort:** M

### CP-13 — Two highlighted rows at once; the default cursor lands on a disabled row
- **Severity:** P2
- **Category:** interaction
- **Where:** `crates/ui/src/popover.rs:783` (`menu_row` hover blends to `card_selected_bg`), `:822` (`menu_row_nav`); compare `crates/ui/src/pickers.rs:7381–7395` (model picker's single moving highlight); `pickers.rs:1212` (`active: 0`), `:5740` (permission rows). Shot: `shots/composer/CP-permission-popover-detached.png`
- **What's wrong:**
  - In the `@`, `#` and `/` popups the keyboard row and a pointer-hovered row both paint `card_selected_bg` (white 11%), so two identical highlights can show at once. `menu_row_nav`'s doc promises a lighter keyboard wash (0.08), but the code applies the same wash to both.
  - The model picker already does it right: hover moves `active`, active is `ink(0.05)`, and selected gets a wash plus a ring.
  - The access menu opens with `active = 0`, which highlights the disabled "Approve for me" row (render above), and the current choice gets no highlight.
- **Why it hurts the premium feel:** Ambiguous selection is one of the most visible differences between a native menu and a web imitation.
- **Proposed fix:** Adopt the model-picker rule in `menu_row`. The pointer and arrow keys share `active`; the active wash is white 8% (#1A1A1A); selection is shown by a trailing check rather than a second wash. The cursor opens on the current choice, and arrow keys skip disabled rows.
- **Before:** `mocks/CP/CP-13-before.svg` (+ shot)
- **After:** `mocks/CP/CP-13-after.svg`
- **Effort:** M

### CP-14 — Seven type sizes and two heading styles across sibling menus
- **Severity:** P2
- **Category:** typography
- **Where:** `crates/ui/src/composer.rs:8915–8933` (files 13 / 12.5), `:9408–9426` (slash 12.5 medium / 12), `crates/ui/src/pickers.rs:7413–7432` (model 12.5 medium / 11), `crates/ui/src/composer/chat_references.rs:741` (badge 10.5), `crates/ui/src/pickers.rs:5718–5790` (permission heading 11 semibold sentence case, descriptions **10pt**, extra `.p(4)` inset), `crates/ui/src/popover.rs:841` (`menu_heading` 10 caps)
- **What's wrong:** Composer menus use the sizes 10, 10.5, 11, 11.5, 12, 12.5 and 13. Primary row text is 12.5/600 in two menus and 13/400 in four. Headings are tracked 10pt caps in the model and chat-reference menus but 11pt semibold sentence case in the access menu. Access descriptions at 10pt fall below the 11pt floor. The permission card double-pads (card 4 + panel 4 + row 8 = 16pt text inset against 12pt everywhere else).
- **Why it hurts the premium feel:** Off-scale half sizes and competing heading styles make sibling menus feel like they came from different apps.
- **Proposed fix:** Add a menu ramp to `typography.rs`: title 13/400 (13/500 for the current item) · secondary 12 muted · footnote 11 (the floor) · badge 11/500 · heading 11/600 tracked caps through `menu_heading()`. Every composer popover uses the single 4pt inset of `popover_card` plus the 8pt padding of `menu_row`.
- **Before:** `mocks/CP/CP-14-before.svg`
- **After:** `mocks/CP/CP-14-after.svg`
- **Effort:** M

### CP-15 — The composer pill has no focused state
- **Severity:** P2
- **Category:** states
- **Where:** `crates/ui/src/composer.rs:90` (`composer_surface` takes no focus input), `:12832` (pill). Shot: parity `c01` (two identical pills)
- **What's wrong:** The pill border is `theme.border` (white 10%) whether or not its input has focus. In a split (main plus side chat, both 108pt pills, identical chips) the only cue for which composer receives keystrokes is a 1.5pt caret. Keyboard focus styles also vary across controls: Goal uses an accent 18% wash, the queue actions use an accent wash, the stash list uses `focus_ring`, and attach, model, access and Send are not tab stops at all.
- **Why it hurts the premium feel:** macOS always shows where typing goes. Ambiguity in a multi-pane workspace is both a polish problem and a correctness risk.
- **Proposed fix:** When focused, the hairline goes to `border_strong` (white 18%) and gains a 3pt accent ring (#8B7CF6 at ~30%), fading over `HOVER_FADE`. The unfocused pill stays unchanged. Use the same ring as the focus-visible style for composer controls (CP-07), and make them tab stops.
- **Before:** `mocks/CP/CP-15-before.svg`
- **After:** `mocks/CP/CP-15-after.svg`
- **Effort:** S

### CP-16 — Floating menus cast no shadow while tooltips do (elevation inverted)
- **Severity:** P2
- **Category:** color/contrast
- **Where:** `crates/ui/src/popover.rs:336` (`.when(!theme.is_frost(), |el| el.shadow_lg())`), compared with `popover.rs:329` and `pickers.rs:591` (tooltips always `shadow_md`); the same pattern repeats in `queue.rs:323`, `composer/goals.rs:1416` and `composer.rs:11991`
- **What's wrong:** In the frosted Luna themes, menus (the top layer) have only a 1px white-10% border and no shadow. Tooltips, which sit a layer lower, carry `shadow_md`. In Luna Light this is most visible: a frosted card over white transcript content has almost no edge.
- **Why it hurts the premium feel:** Elevation is how macOS tells layers apart. A flat top layer reads like an inline panel rather than a menu.
- **Proposed fix:** Menus always get elevation, frost or not: 0 10 24 /16% + 0 1 2 /10% in light, about /45% in dark. Tooltips sit one step lower at 0 4 10 /12%. Put both in shared tokens so trays and cards can opt in deliberately.
- **Before:** `mocks/CP/CP-16-before.svg` (Luna Light)
- **After:** `mocks/CP/CP-16-after.svg`
- **Effort:** S

### CP-17 — Unknown context usage renders as "○ —"
- **Severity:** P2
- **Category:** states
- **Where:** `crates/ui/src/context_usage.rs:23–32` (`label` returns "—"), `:36–38` (ring 16 slot, radius 6, stroke 1.8), `:113–123` (h24 r6, 11pt)
- **What's wrong:** Every chat without reported usage, which is every fresh chat in every capture (parity, chat-references, goal, queue), shows an empty ring followed by an em-dash. The ring is 12pt drawn in a 16pt slot with a 1.8pt stroke, inside an h24 r6 chip that differs from every neighbour (CP-07).
- **Why it hurts the premium feel:** A dash placeholder reads as a missing value or a glitch, permanently visible in the main row.
- **Proposed fix:** When usage is unknown, show a track-only ring with no label; the tooltip already explains why. When known, show an arc plus a tabular % label, with warning at 75% and danger at 90% using the existing tones. Use a 14pt ring with a 2pt stroke in the shared 28pt capsule and a 12/600 label.
- **Before:** `mocks/CP/CP-17-before.svg`
- **After:** `mocks/CP/CP-17-after.svg`
- **Effort:** S

### CP-18 — The text column and the control column start at different x
- **Severity:** P2
- **Category:** alignment
- **Where:** `crates/ui/src/composer.rs:424` (`input_side_padding` 16), `:392` (`ACTIONS_ROW_INSET` 12), `:12805` (paperclip `+1` optical nudge). Shot: `shots/composer/CP-composer-idle-dark.png`
- **What's wrong:** Measured on parity `c01` at 2x, the placeholder ink starts 18pt from the pill's outer edge (1 border + 16 padding + ~1 bearing). The paperclip ink starts at 21.5pt (1 + 12 inset + 6 centring + 1 nudge + ~1.5 glyph whitespace), a 3.5pt step between the two columns.
- **Why it hurts the premium feel:** Apple layouts hang text and controls from one leading edge. A small offset like this is subliminal but makes the composer feel slightly off.
- **Proposed fix:** Change `ACTIONS_ROW_INSET` from 12 to 10 and drop the +1pt nudge, so the paperclip ink lands at about 18pt. Send's trailing inset becomes 10pt as well, which keeps the row symmetric. Add a `COMPOSER_LEADING` constant and a probe assertion in the parity fixture.
- **Before:** `mocks/CP/CP-18-before.svg`
- **After:** `mocks/CP/CP-18-after.svg`
- **Effort:** S

### CP-19 — Attachment remove control: 18pt, hover-only, double ring; thumbnail radii drift
- **Severity:** P2
- **Category:** sizing
- **Where:** `crates/ui/src/composer.rs:8307–8333` (remove button), `:8275` (strip thumbnail r8), `crates/ui/src/queue.rs:1161` (queue thumbnail 40×28 r5), `composer.rs:8399` (appshot tile r14)
- **What's wrong:** The remove button is an 18pt disc at −6/−6. It is invisible until hover (`opacity(0)`, then `group_hover` to 1 with no fade) and has no tab stop, so keyboard users cannot remove an image. It draws a `CLOSE_CIRCLE` glyph inside a filled `theme.bg` disc, which shows as two concentric rings. Thumbnail radii in the same flow are 8, 5 and 14.
- **Why it hurts the premium feel:** The target is too small to hit reliably, the control is hard to discover, and the glyph looks doubled.
- **Proposed fix:** A 20pt #2C2C2E disc with a 1px ring and a plain 9pt × glyph (stroke 1.75), with a 28pt hit area. It fades in over `HOVER_FADE` on hover *and* on keyboard focus, and Delete or ⌫ removes the focused thumbnail. Make the radii concentric: strip 10, queue 6, appshot 14.
- **Before:** `mocks/CP/CP-19-before.svg`
- **After:** `mocks/CP/CP-19-after.svg`
- **Effort:** S

### CP-20 — Stacked cards above the composer use four unrelated radii
- **Severity:** P2
- **Category:** consistency
- **Where:** `crates/ui/src/notice.rs:26` (notice r10), `crates/ui/src/composer.rs:12500` (recover card r10), `crates/ui/src/popover.rs:302` (full-width completion popup r12), `crates/ui/src/queue.rs:101` and `composer/goals.rs:77` (trays r16), `composer.rs:80` (pill r26). Shots: `shots/composer/CP-chat-references-popup.png`, `shots/composer/CP-draft-notice.png`
- **What's wrong:** One vertical stack can show a notice (r10), a full-width popup (r12, exactly as wide as the pill), a queue tray (r16) and the pill (r26). The full-width popup's tight 12pt corners directly above the 26pt pill are the most jarring pair.
- **Why it hurts the premium feel:** Apple keeps radius families concentric. Unrelated radii in a stack read as parts from different kits.
- **Proposed fix:** A radius ladder of pill 26 → full-width popups 20 (rows r14, inset 6) → trays 18 (16pt inset) → notices 14. Add a `CARD_RADIUS_WIDE` frost mask for full-width completions; anchored menus keep 12.
- **Before:** `mocks/CP/CP-20-before.svg` (+ shots)
- **After:** `mocks/CP/CP-20-after.svg`
- **Effort:** S

### CP-21 — Queue row: tiny drag handle, r5 buttons, instant hovers, mixed action styles
- **Severity:** P2
- **Category:** sizing
- **Where:** `crates/ui/src/queue.rs:794–811` (handle 14×22), `:1223–1272` (actions 28 r5, opacity 0.72, instant `ink(0.07)`), `:1287–1340` ("Steer now" 11.5pt text in a 72pt slot), `:95–105` (row 36, text 12.5, icons 13). Shot: `shots/composer/CP-queue-tray.png`
- **What's wrong:** The drag handle's hit area is 14×22pt. The row actions use radius 5 (everything else in the composer uses 8 or capsules), rest at 72% alpha, and hover instantly. "Steer now" is bare text next to icon buttons. Text is 12.5pt and icons 13pt, both off the menu ramp.
- **Why it hurts the premium feel:** The queue sits directly on the composer, so its controls should feel like the composer's own.
- **Proposed fix:** Give the handle a 20×28pt hit area with a 14pt glyph. Make the icon actions `composer_control()` capsules (CP-07) at rest colour #A9A9AE, using colour instead of alpha. Turn "Steer now" into a 24pt capsule text button (12/600). Use the shared tooltip (CP-12), row text 13 and icons 14.
- **Before:** `mocks/CP/CP-21-before.svg` (+ shot)
- **After:** `mocks/CP/CP-21-after.svg`
- **Effort:** S

### CP-22 — Goal bar: orphan separator dot and a jittering timer
- **Severity:** P3
- **Category:** typography
- **Where:** `crates/ui/src/composer/goals.rs:1139–1180` (objective `flex_1`; the timer and checks each start with a "·"). Shot: `shots/composer/CP-goal-bar-orphan-dot.png`
- **What's wrong:** With a short objective ("yo what is this") the timer's leading "·" floats far right with nothing before it to separate. The live timer uses proportional digits, so its width changes every second (for example "11m 11s" is narrower than "10m 00s") and the edit, pause and delete controls shift.
- **Why it hurts the premium feel:** These are small, but they move every second in the user's peripheral vision.
- **Proposed fix:** Put the separator only between the title and the objective. Make the timer right-aligned trailing metadata with tabular digits (the mono face, as `change_requests.rs:176` already does for PR numbers).
- **Before:** `mocks/CP/CP-22-before.svg` (+ shot)
- **After:** `mocks/CP/CP-22-after.svg`
- **Effort:** S

### CP-23 — The draft notice repeats its title and spends a row on a text link
- **Severity:** P3
- **Category:** spacing
- **Where:** `crates/ui/src/composer.rs:6417` (message), `:12449` (title), `:12457–12480` (action row). Shot: `shots/composer/CP-draft-notice.png`
- **What's wrong:** The title is "Draft not saved" and the body begins "Draft not saved. Your text is still here…". "Retry save" is bare red text on its own right-aligned row, which makes the card 82pt tall for one sentence.
- **Why it hurts the premium feel:** Redundant copy and a link-style action read as unfinished next to the composer.
- **Proposed fix:** One 40pt line: ⚠ title · reason · a 24pt capsule "Retry" · copy. Radius 14, matching CP-20. Drop the duplicated sentence from `composer.rs:6417` and `:6506`.
- **Before:** `mocks/CP/CP-23-before.svg` (+ shot)
- **After:** `mocks/CP/CP-23-after.svg`
- **Effort:** S

### CP-24 — Five popover widths and three hairline alphas
- **Severity:** P3
- **Category:** consistency
- **Where:** `crates/ui/src/pickers.rs:5112–5356, 8361, 8368` (widths 224, 280, 304, 320, 340), `crates/ui/src/popover.rs:870` (`menu_separator` 0.07), `:1053` (`menu_section` 0.06), `pickers.rs:6109` (model tabs border 0.08)
- **What's wrong:** Every call site picks its own width, so a row of sibling chips opens five differently sized cards. Inside them, dividers use three different alphas.
- **Why it hurts the premium feel:** These are minor individually, but they add to the "assembled" feel that CP-07 and CP-14 describe.
- **Proposed fix:** Add `MENU_W_S` 240 / `MENU_W_M` 280 / `MENU_W_L` 320 to `popover.rs`. Content may widen a card but never shrink it below its token. Use one `MENU_HAIRLINE` = 0.08.
- **Before:** `mocks/CP/CP-24-before.svg`
- **After:** `mocks/CP/CP-24-after.svg`
- **Effort:** S

---

## Top 5 fixes with the biggest premium payoff

1. **One composer control system (CP-07, plus CP-17 and CP-21).** A single `composer_control()` capsule (28pt, faded hover, 16pt icons, 12/600 labels) turns the app's most-used row from six components into one.
2. **Anchor everything to its trigger (CP-01, CP-02).** Menus open leading-aligned from their chip, and tooltips sit centred above their control. Together these remove the two most visible "this is broken" moments in the composer.
3. **Popover motion done properly (CP-08, CP-11, CP-10).** Direction-aware 180ms ease-out-quint entrances with blur riding the same progress, exits for completions, and a send→stop glyph morph. Every menu open and every send then feels native.
4. **A real tonal hierarchy in the composer (CP-03).** A `for_composer()` ladder floored on the actual plate brings the placeholder back to #85858A and makes the model name lead, so the empty composer finally looks empty.
5. **One tooltip, one highlight model, one menu type ramp (CP-12, CP-13, CP-14).** Every hover card, menu row and heading starts reading as one design system.
