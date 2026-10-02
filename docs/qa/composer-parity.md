# Composer parity: main, side and child chats

Side chats, child chats and the main chat use one composer geometry, modelled on the Codex desktop app. The headless design fixture and merged native app have been checked. Native findings and corrective verification are recorded in [the October2 QA follow-up](codex-native-2026-10-02.md).

## Design

- **One layout.** Every established chat uses the two-row pill: the input above, the controls row pinned to the pill's bottom. The single-line compact mode and its width-driven flip are gone, so a main chat no longer changes shape while its side pane stays two-row. An empty session pill is 108 px (one input line plus the 46 px controls row). New chats keep the 124 px blank-canvas pill and morph between the two on the route transition.
- **Same position.** The pane composer has no inset of its own and uses the main column's 16 px gutter and bottom margin, so both pills and their footers sit at the same y.
- **No cut text.** The controls row measures its chips with the text system and drops whole pieces in priority order: first the Goal label (leaving its icon), then the effort suffix, then the model name (leaving the brand mark). The access chip always stays. Every dropped piece remains in a tooltip.
- **Model chip.** It shows the brand mark, the model name and the reasoning level. A fast speed shows a bolt after the name instead of "Fast". The tooltip spells out the full configuration ("High · 200K · Fast").
- **Speed.** Codex's service tier and Claude's fast mode are both labelled **Speed: Standard / Fast** in the provider catalogs. Stored choice ids are unchanged (`serviceTier` default/fast, `fastMode` off/on).
- **Access chip.** The chip is icon-only (Solar Linear icons) with a tooltip:
  - Limited: closed lock.
  - Approve for me: shield check.
  - Full access: open lock in the warning tone.
  - Choice not yet made: shield warning.
  - A requested level that hasn't applied yet keeps the effective icon and adds a corner dot.
- **Independent composers.** Hover fades are keyed per picker entity. Before, every composer shared keys such as `picker-model`, so hovering the main chips lit the side chat's too. Model, effort, speed and access changes were already written to the bound chat only; the fixture now asserts it.
- **Side chat header.** A side chat inherits checkout, access and provider from the main chat beside it, so its header is one row: Back (all side chats), the title and Close (Unload). The lease time, parent and provider facts move to the title's tooltip, and the title reveals where the chat started. Only non-live states (saving, read-only) stay visible. Child chat headers are unchanged.

## Fixture

`composer-parity-fixture` drives the production `Shell` against an in-process engine with a scripted provider that has a Claude-shaped catalog. The provider keeps the Mock id: the Claude Code id makes the engine probe the real `claude` CLI for its catalog. Its failure cases are listed in the example's header. The fixture asserts each case and captures c01–c11 (dark and light; 520/360/300/760/480 px panes and a forced 240 px pane).

```sh
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture --example composer-parity-fixture
target/debug/examples/composer-parity-fixture ABSOLUTE_EVIDENCE_DIR
```

`composer-parity.pass` marks success; the process exit code is not a reliable signal.

## Limits

- The Goal switch is offered only on Codex and Claude Code chats, so it does not render in this fixture. Its icon-only fallback is covered by the controls-row fit, which measured 67/32 px in an earlier Claude-id run.
- Real provider catalogs, native window resizing, VoiceOver and pointer tooltips in the shipped app still need a native pass.
