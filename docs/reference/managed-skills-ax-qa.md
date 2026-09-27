# Managed skill controls: native accessibility acceptance

## Failure-first trace

On the signed isolated `LUNA QA Skills` build from source `8290c6c2`, the
Providers Settings screen visibly showed the OpenCode skill library with
`New global skill`, `New project skill`, `Inspect / edit`, and the saved
`qa-opencode-skill`. The macOS accessibility readback for the same window had
the Settings sidebar buttons and the two LUNA instruction text areas, but no
skill-library buttons. While the skill editor was visible, the readback added
`SKILL.md content` as a text area but still omitted `Close`, `Save skill`,
destination provider/scope, `Install copy`, and `Remove`. The visible controls
could only be exercised by coordinates in this QA pass.

That red readback is recorded in the task's `cua_repl` screenshot and AX tool
results. Synthetic roots, exact app/process readback, behavioral results, and
the limitation are recorded at
`target/qa/skills-native-8290c6c2/native-qa.txt` in the integration checkout.

## Acceptance on the refreshed build

1. On Providers Settings, native AX must name `Check provider`, all three
   provider choices, both New skill actions when a project is selected, and
   `Inspect` for the listed skill. Choices report selection state.
2. When a skill is open, native AX must name `Close`, `Save skill`, all three
   install-provider choices, the eligible install scopes, `Install copy`, and
   `Remove skill` or its armed confirmation label. Disabled actions do not
   enter the tab order.
3. Tab focuses each enabled control with Luna's existing focus-visible border;
   Enter or Space activates it once on key-up through GPUI's synthesized
   click. Do not add another key-down handler.
4. Read the same controls in both Light and Dark appearances. Confirm System
   is restored afterward, no unsaved editor remains, and only the synthetic
   provider roots were mutated.

This is a rendered/native AX acceptance trace, not a unit test. The existing
`command_settings_button` pattern in `crates/ui/src/project_commands.rs`
provides the role, label, tab, and focus behavior for ghost actions.
