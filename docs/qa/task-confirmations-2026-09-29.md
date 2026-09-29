# Task confirmation native QA — 29 September 2026

The signed primary **LUNA Dev** app exercised archive, settle, and delete on
three task records created in an owned disposable project. The real provider
run used Codex `gpt-6-luna` at **High** reasoning and returned the exact marker
`LUNA_CONFIRM_QA_ec9717d737` without tools. The app was refreshed after the
final code edit in **15 seconds**. PID `12889` owned `127.0.0.1:59777` from the
exact LUNA Dev bundle, and `codesign --verify --deep --strict` passed. Final
binary SHA-256: `10b16395e1fff36f87e3c6dc7b9e5ba519ee1e37072dadd62ba53b52b10e9b8f`.

## Results

| Flow | Native UI and durable readback |
| --- | --- |
| Archive confirmation | Dialog opened immediately with the current glass surface, border, font, and buttons. Cancel kept the task active. Confirm moved it out of Active on the next UI read and `WatchChats` reported `archived: true`. |
| Archive skip | Checking “Don't show this again” before confirming turned only Confirm archive off. After unarchiving the same task, a second Archive click showed no dialog and the task was archived again. |
| Settle confirmation | The dialog opened immediately. Confirm moved the idle task to Settled on the next UI read, and the task record gained a `settled` override. |
| Settle skip | The checkbox turned only Confirm settle off. After Return to Active, a second Settle click showed no dialog and moved the task directly to Settled. |
| Completed provider task | The completed GPT-6 Luna task was unarchived, manually settled, and remained in the Settled shelf after the authoritative override arrived. This covers the previous Completed-versus-Idle classification gap. |
| Delete | The dialog kept the detailed permanent-deletion warning and used the theme's danger button. Checking the box then cancelling left the task and Confirm delete unchanged. The General switch toggled off and back on with persisted readback. The disposable delete task was removed through `ExecuteTaskCommand`; its receipt was `applied`, and a fresh `WatchChats` read found it absent. |
| Appearance and restart | The delete dialog was visually inspected under Luna Dark and Luna Light. In both modes it used the shared modal/card/button styles and readable theme text. The original Dark appearance and all three confirmation switches were restored. A final app refresh preserved the switches and task states. |

The final cleanup left the two remaining QA tasks archived and the disposable
delete task absent. The QA project and its workspace remain under the app's
records and `target/qa/` so the result can be inspected; no existing user task
was altered. Native UI QA did not press the irreversible Delete button because
computer-use policy requires action-time confirmation for permanent deletion.
The shared confirmation dispatch was exercised by Archive and Settle, and the
delete engine command was checked independently.

## Repeatable evidence

Run `node scripts/qa-task-confirmations.mjs setup target/qa/<unique-run>` with
the signed primary LUNA Dev app running. The setup creates a disposable Git
workspace, three real task records, and one exact-marker GPT-6 Luna High turn.
Use `snapshot target/qa/<unique-run> <unique-label>` after each native UI step.
`delete-fixture target/qa/<unique-run>` deletes only that run's recorded delete
task and refuses replay. The UI action sequence is in the results table above.

This run's JSON evidence is in `target/qa/task-confirmations-2026-09-29/`:
`manifest.json`, `provider-result.json`, the labeled `snapshot-*.json` files,
and `delete-receipt.json`. Selected SHA-256 readbacks:

| Artifact | SHA-256 |
| --- | --- |
| `provider-result.json` | `d5700177c75cd63aeec8b8cb7ef32665a42ccfc9ddcc092310f80caef13bbf9f` |
| `snapshot-provider-completed-settled.json` | `28ecf9f20d763588c59f8f71845a273a3a4751901086cc4d8b0ec4c32c471f69` |
| `snapshot-post-final-refresh.json` | `d0f5c99521fe80bafb5472b4f7cf0213d65dae01de063f2d6d23b0edc266b0bc` |
| `delete-receipt.json` | `0aebf222683caa865ba38f20b44f7c001e150be636806d54a8b7d88d4062c1ba` |

Verification: `scripts/cargo.sh check --locked -p luna-ui`,
`scripts/cargo.sh clippy --locked -p luna-ui`,
`scripts/cargo.sh fmt --all -- --check`, `node --check` for the QA driver, and
`git diff --check` passed. Clippy reported workspace warnings; it
exited successfully. Final storage: `target/debug` 49 GiB,
`target/release` 3.2 GiB, 366 GiB free.
