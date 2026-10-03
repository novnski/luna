# UI keyboard activation, Escape and focus-ring cases

Native acceptance for the 2026-10-03 UI audit (keyboard double activation, modal
Escape, layout-neutral focus rings, pointer-only actions). Evidence is collected
by `scripts/qa-ui-keyboard.mjs` from a dedicated QA bundle. Input is pressed in
the real app, and the script only reads `ui-settings.json`, the engine over IPC
and window captures.

## How each control can fail

1. **Double activation.** A focused `on_click` control that also acts on an
   Enter/Space key-down fires twice: GPUI adds a keyboard click on key-up.
   - Switches and checkboxes end unchanged.
   - Cycles skip a value.
   - One-shot actions (import, insert, remove, retry) run twice or hit the
     next item.
2. **No activation.** A key-down handler swallows Enter/Space but the element
   has no `on_click`, no tab stop, or loses focus before key-up.
3. **Wrong toggle direction.** A menu trigger that decides open/close from a
   mouse-down press note reopens on a keyboard click.
4. **Mouse regression.** Pointer clicks stop working or fire twice after the
   keyboard change.
5. **Escape trap.** A modal's Escape is consumed by the Shell capture phase
   without closing it, and its buttons are not tab stops.
6. **Escape side effect.** Escape on a confirmation confirms it, interrupts the
   agent, or clears the composer draft.
7. **Focus moves layout.** A focus-only border grows an auto-sized row (it is
   applied during layout) and shifts its neighbours, or nudges a switch knob.
8. **Unreachable or unannounced.** An action has no tab stop, role or label.
   Recovery trailer, failed-send retry, file banners, settings actions and
   dialog buttons are the critical ones.
9. **Persistence.** A toggled setting is not written to `ui-settings.json` (the
   400 ms debounce) or another key changes with it.

## Cases

| Case | Control | Pass criteria | Evidence |
|---|---|---|---|
| K1 | Files → Word wrap switch | Space flips `filesWordWrap` once; Enter flips it back once; click flips once; no other key changes | `flip` ×3 |
| K2 | General → Show context window usage | as K1 for `showContextUsage` | `flip` ×3 |
| K3 | Appearance → Chat code wrapping | as K1 for `codeFencesFitContent` | `flip` ×3 |
| K4 | Files → autosave delay radio | Enter selects one delay; `filesAutosaveDelayMs` changes once | `same`/diff of `snap` JSON |
| K5 | Appearance → font size dropdown | Enter opens; Arrow + Enter commits; menu does not reopen | PNGs |
| K6 | Sidebar view options / theme menu triggers | Enter opens; Enter on trigger closes | PNGs |
| E1 | Archive confirm | Escape closes; chats and sessions unchanged | `same --engine` |
| E2 | Remove project / Delete / Sign out confirm | as E1 | `same --engine` |
| E3 | Dialog buttons | Tab reaches Cancel/confirm; Enter on Cancel closes once | PNGs |
| L1 | Settings nav row focus | changed-pixel box ≤ 40pt tall, inside the row | `pixels --expect-change` |
| L2 | General switch focus | as L1 | `pixels --expect-change` |
| L3 | Dialog button focus | as L1 | `pixels --expect-change` |
| R1 | L10 recovery trailer "Cancel" | Tab reaches it; Space cancels once | fixture log + PNG |
| R2 | L10 recovery trailer "Send now" | one `retryRecovery`; attempt +1 | fixture log + PNG |
| F1 | File outside-change banner | Tab reaches "Keep editing"/"Reload from disk"; one activation; sentence case | PNGs |

## Procedure

Run only one Luna GUI at a time. Let primary LUNA Dev save and quit gracefully
first; never force-quit it. Build the QA bundle with the QA identity overrides
(see `docs/reference/qa-infrastructure.md`), then verify the process, port and
signature:

```bash
export LUNA_DEV_APP_NAME='LUNA QA UI'
export LUNA_DEV_BUNDLE_ID='com.novnski.luna.qa.ui'
export LUNA_DEV_IPC_PORT=59788
export LUNA_DEV_DATA_DIR="$PWD/target/luna-qa-data/ui"
RUN=target/qa/ui-keyboard-$(date +%Y%m%d-%H%M%S)
node scripts/qa-ui-keyboard.mjs preflight "$RUN"
# Operator automation supplies every capture (no osascript/screencapture):
# node scripts/qa-ui-keyboard.mjs preflight "$RUN" --external-capture
# then each snap passes --image <capture>; pixels uses --scale for its px/pt.
```

For each case:

1. `snap <label>` before the input.
2. Press the keys in the app.
3. `snap <label>` after.
4. Assert with `flip`, `same` or `pixels`.

For example, K1:

```bash
node scripts/qa-ui-keyboard.mjs snap "$RUN" k1-start
# Tab to Word wrap, press Space
node scripts/qa-ui-keyboard.mjs snap "$RUN" k1-space
node scripts/qa-ui-keyboard.mjs flip "$RUN" k1-start k1-space filesWordWrap
```

Use `snap <label> --engine` around Escape cases. R1/R2 run in the L10 fixture
(`scripts/run-macos-l10-fixture.sh`, mode `turn-recovery`) once the QA app has
quit. Finish with `summary`, which writes `summary.json` with the sha256 of
every artifact. After review, quit the QA app gracefully, unregister and remove
only its bundle, keep its data and the Cargo cache, and reopen primary LUNA Dev.
