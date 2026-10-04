# Composer parity: main, side and child chats

Side chats, child chats and the main chat use one composer geometry, modelled on the Codex desktop app. The headless design fixture and native app have been checked. Current provider and context-meter verification is recorded in [the October 4 follow-up](../design/workbench-followup-2026-10-04/verification.md); earlier findings are in [the October 2 QA follow-up](codex-native-2026-10-02.md).

## Design

- **One layout.** Every established chat uses the two-row pill: the input above, the controls row pinned to the pill's bottom. The single-line compact mode and its width-driven flip are gone, so a main chat no longer changes shape while its side pane stays two-row. An empty session pill is 108 px (one input line plus the 46 px controls row). New chats keep the 124 px blank-canvas pill and morph between the two on the route transition.
- **Same position.** The pane composer has no inset of its own and uses the main column's 16 px gutter and bottom margin, so the pills' lower edges align. Workbench composers have no separate context-usage footer.
- **No cut text.** The controls row measures its chips with the text system and drops whole pieces in priority order: first the Goal label (leaving its icon), then the context percentage (leaving the ring), then the effort suffix, then the model name (leaving the brand mark). The access chip always stays. Every dropped piece remains in a tooltip or details card.
- **Model chip.** It shows the brand mark, the model name and the reasoning level. A fast speed shows a bolt after the name instead of "Fast". The tooltip spells out the full configuration ("High · 200K · Fast").
- **Speed.** Codex's service tier and Claude's fast mode are both labelled **Speed: Standard / Fast** in the provider catalogs. Stored choice ids are unchanged (`serviceTier` default/fast, `fastMode` off/on).
- **Access chip.** The chip is icon-only (Solar Linear icons) with a tooltip:
  - Limited: closed lock.
  - Approve for me: shield check.
  - Full access: open lock in the warning tone.
  - Choice not yet made: shield warning.
  - A requested level that hasn't applied yet keeps the effective icon and adds a corner dot.
- **Context usage in the controls row.** The context-window ring and share sit after Goal in the controls row (Main, child and fork composers alike), with the same details card and an accessible name. The row's fit gives the share up (ring stays) after the Goal label and before the model's effort. In the workbench frame nothing remains under the pill, so the composer sits lower.
- **Independent composers.** Hover fades are keyed per picker entity. Before, every composer shared keys such as `picker-model`, so hovering the main chips lit the side chat's too. Model, effort, speed and access changes were already written to the bound chat only; the fixture now asserts it.
- **Child and fork headers.** The tab strip carries identity, live progress and attention state; no status/source/model caption sits below it. Parent/source navigation is available through the tab context menu and titlebar where relevant. Selected tabs use a rounded fill, with an accent tint for the focused group. Forks follow their own settings and have no End, lease, expiry or side-only read-only state.

## Fixture

`composer-parity-fixture` drives the production `Shell` against an in-process engine with a scripted provider that has a Claude-shaped catalog. The provider keeps the Mock id: the Claude Code id makes the engine probe the real `claude` CLI for its catalog. Its failure cases are listed in the example's header. The fork is created with New side chat and moved into a split beside Main (Move tab to split right), so the two composers sit in two workbench groups. The fixture asserts each case and captures c01–c11 (dark and light; 520/360/300/760/480 px right sidebars, and the fork's group at its 320 px minimum, which replaced the old forced 240 px pane: groups never shrink below 320 px).

```sh
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture --example composer-parity-fixture
target/debug/examples/composer-parity-fixture ABSOLUTE_EVIDENCE_DIR
```

`composer-parity.pass` marks success; the process exit code is not a reliable signal.

## Limits

- The Goal switch is offered only on Codex and Claude Code chats, so it does not render in this fixture. Its icon-only fallback is covered by the controls-row fit, which measured 67/32 px in an earlier Claude-id run.
- Native cross-provider catalogs, independent child selection, restart and delivery are covered by the October 4 follow-up. VoiceOver and native pointer-tooltip content were not asserted in that run.
