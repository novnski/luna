# General settings QA — 2026-09-25

## Source inventory

- Codex: the user-supplied 12.85-second General-settings recording shows composer send shortcut, follow-up behavior, context-window usage, web/file and workspace defaults, app layout, sleep prevention, notifications, and Codex-specific controls.
- BB: `get-bb/bb` at `9c8112e`, `apps/app/src/views/SettingsView.tsx`. Its General section contains navigate-after-create, rich-text editing, follow-up behavior, in-app links, localhost rewrite, and managed branch prefix. Device, privacy, appearance, and keyboard settings are separate.
- T3 Code: `pingdotgg/t3code` at `e5a46d6c5`, `apps/web/src/components/settings/SettingsPanels.tsx`. Its General panel includes organization, notifications, composer send shortcut, follow-up behavior, rich text, background activity, and project defaults.
- LUNA: General previously exposed follow-up behavior and automatic retries. LUNA already had a send-shortcut setting under Shortcuts, a web-link preference in transcript menus, and a context-usage indicator in the composer.

## Candidate mapping

| Reference setting | LUNA decision in this pass |
| --- | --- |
| Send shortcut (Codex, T3) | Added to General and synchronized with the existing Shortcuts control and active composer keymap. |
| Follow-up behavior (Codex, BB, T3) | Kept the existing Queue/Steer control and persistence. |
| Context usage (Codex) | Added a visibility switch for the existing composer indicator. |
| Open links inside the app (BB) | Added a General switch for the existing LUNA browser preference. |
| Prevent sleep while running (Codex) | Added a global macOS switch using LUNA's existing process-local power assertion; per-task selections still work when global is off. |
| Automatic retries (LUNA) | Kept the existing control and made its disk update immediate for the engine's failure path. |
| Notifications (Codex, T3) | LUNA already provides a dedicated Notifications category with event and task controls. |
| Rich/plain text composer (Codex, BB, T3) | LUNA's composer remains plain text; a visual switch needs a separate rich-text editing feature. |
| Task folder, file opener, terminal placement, menu bar, popup window, speed, permissions (Codex) | These require separate LUNA behavior or provider-specific design before a truthful control can be exposed. |
| Project grouping, defaults, branch prefix, background activity (BB, T3) | These affect LUNA's project and execution model and need a separate scoped design pass. |

## Repeatable live check

1. Run `cargo fmt --all -- --check`, `cargo check --locked -p luna-ui`, and `cargo check --locked -p luna-ui --tests` from the repository root. The test-target command compiles existing tests; it does not run them.
2. Run `scripts/run-macos-dev.sh --refresh`. Open LUNA Dev → Settings → General. The accessibility tree should show Send shortcut, Follow-up behavior, Show context window usage, Open web links in LUNA, Automatic retries, and (on macOS) Prevent sleep while running.
3. Select modifier-plus-Enter, Queue, context usage off, LUNA links off, automatic retries off, and prevent sleep on. Verify each control's selected/toggled accessibility value. Read `target/luna-macos-dev/data/ui-settings.json` and expect `mod-enter`, `queue`, `false`, `false`, `false`, and `true` respectively under the corresponding camel-case keys.
4. Run the refresh command again, reopen General, and verify the same six UI values. This proves settings survive app and engine restart. The observed second build took 1 second.
5. Restore the prior values through the UI: Enter, Steer, context usage on, LUNA links on, automatic retries on, prevent sleep off. Confirm the saved JSON matches.
6. Verify the launched executable belongs to `target/luna-macos-dev/LUNA Dev.app`, `127.0.0.1:59777` listens, and `codesign --verify --deep --strict 'target/luna-macos-dev/LUNA Dev.app'` succeeds.
7. For a composer effect check, select modifier-plus-Enter again, return to a fresh task canvas, type a unique disposable draft, and press plain Enter. In the local-only development profile, read the matching row in `composer_drafts` from `target/luna-macos-dev/data/profiles/local/docs.sqlite3` with SQLite read-only mode. The `record_json` text should contain the draft followed by `\n`; this confirms Enter inserted a line break. Clear the draft, confirm the matching row disappears, and restore Enter-to-send.
8. Turn on Prevent sleep while running and start a disposable local task that runs `sleep 20` without editing files. While its run is active, `pmset -g assertions` should show `NoIdleSleepAssertion named: "LUNA selected active task"` under the exact LUNA Dev PID. After the run completes, the assertion should disappear. Restore the switch to off.

## Observed result

The live accessibility tree showed all six changed states before and after the restart. The JSON file matched both the changed values and the restored baseline. The first refresh built in 38 seconds; the restart built in 1 second. The exact bundle process, local IPC listener, and signature all passed readback.

The composer probe was saved as `LUNA QA newline probe 2026-09-25\n` after plain Enter while modifier-plus-Enter was selected. The draft remained unsent. Clearing it removed the matching saved row, and Enter-to-send was restored.

During the disposable task, `pmset -g assertions` showed the LUNA assertion under PID 31381. After the 20-second run, the assertion disappeared; the switch was restored to off.

The link handler and recovery engine are wired to these settings in source. The live check did not obtain a reliable full-size screenshot of the composer after macOS presented the LUNA window as a thumbnail. External versus in-app link opening was not exercised with a live transcript link in this pass.
