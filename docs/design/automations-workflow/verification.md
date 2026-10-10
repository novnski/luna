# Automation workflow verification — 2026-10-10

Codex implemented the engine/protocol/storage changes; Claude Desktop implemented
the macOS UI. This closes the workflow gaps identified in the October 9 review.
The evidence directory is `target/qa/automations-workflow-2026-10-10/`; the original
review is retained in `target/qa/automations-review-2026-10-09/`.

## Delivered behavior

| Review findings | Result |
| --- | --- |
| A01, A07 | Each automation saves its provider, account, model, reasoning and model options independently of later source-chat edits. |
| A02, A04, A06, A08, A09, A11 | Project-scoped creation, filtering and retargeting; fresh chats can outlive their source; removing a project blocks delivery. All supplied read filters intersect. |
| A03 | Invalid dates/times are rejected instead of wrapping into valid dates. |
| A05 | Unknown outcomes have an explicit reviewed Resolve action. It acknowledges the old occurrence without replaying it; live turns cannot be resolved. |
| A10 | Successful agent automation tools render a live inline card. Scheduled user messages carry a distinct attribution badge. |
| A12, A13 | Saved notification preferences control banners/sounds while Activity retains outcomes. Fresh successful chats can archive automatically without hiding later human work. |
| A14 | Custom minute-precision recurrence supports bounded calendar rules and host-computed next-run previews. |
| A15–A17 | Edits preserve paused state; successful one-shots become Finished; deleted history remains accessible. |
| A18 | Codex, Claude Code and OpenCode can run project automations. |

Additional verification fixed an asynchronous model-catalog review race, a Unicode
weekday-description panic, vertical overlap in the embedded editor, and a scheduler
race that mistook pending provider acknowledgement for closed uncertainty. An
unchanged execution selection also ignores unrelated composer checkout defaults.
Saved folders and known checkout identities are rechecked before delivery, and
historical recipients do not gain access to a definition moved to another project.

## Verification results

- Engine automation E2E: **29 passed**, one opt-in live GitHub check ignored.
  `engine-tests-final-2.log` and `engine-final/*.json` cover actual RPC, SQLite,
  task admission, provider protocol events, recurrence, permissions and recovery.
- Adjacent engine checks: **9 passed** (Activity, Claude MCP and cross-task tools),
  with one opt-in Activity performance test ignored; `engine-tests-3.log`.
- Protocol/document/storage library checks: **203 passed**;
  `storage-contract-tests.log`.
- UI notification policy and notification-target routing checks: **2 passed**;
  `ui-policy-tests.log` and `ui-banner-tests.log`.
- Affected-library Clippy and changed-file formatting checks passed. Compiler and
  Clippy warnings remain; the dev linker also reports its large unwind-table warning.
- Rendered production UI fixture: **passed**, including the automation cases,
  inline cards, pane geometry and theme/layout stages;
  `ui-e2e-6/chat-workbench.pass`, `readback.json` and PNG captures.
- Final signed-app provider matrix: **6/6 passed**, followed by a passing
  restart/no-replay readback on the final scheduler code;
  `native-final/automations/run-sjJ3bV/{result,restart}.json`.

The first native matrix passed all six combinations of provider and destination,
and restart preserved exactly one completed occurrence with no paused deliveries.
Evidence: `native/automations/run-cwj2tw/` and
`native-opencode/automations/run-6mPWjf/`. Codex used `gpt-6-luna` / High; Claude
Code used `claude-sonnet-5-5` / High; OpenCode used `opencode/big-pickle`, which
advertised no reasoning levels. Recorded runtime settings describe what the engine
admitted; they are not an independent provider echo of every setting.

Native mouse/keyboard QA verified creation, model/reasoning picks, existing-chat
provider mismatch feedback, save/reopen, pause, rename while paused, notification
selection, archive selection, malformed recurrence rejection, Resolve, deletion and
Show deleted/History. Backend readbacks are `native-ui-readback.json` and
`native-ui-final-readback.json`. The Unknown occurrence used for the native Resolve
control was explicitly synthetic in the stopped isolated QA profile; real unknown
runtime behavior is covered by engine E2E. The readback confirms its old message was
not sent and its history remains after deletion.

`native-pane-overlap-before.png`, `native-pane-after.png` and
`native-pane-actions-after.png` record the native light-theme layout repair and
scrollable actions. The rendered fixture also checks the embedded pane geometry.

## Repeat the automated checks

Run from the repository root, with writers idle and the normal shared build guard.
Only one LUNA GUI app may run; quit it gracefully before the rendered fixture.

```sh
LUNA_AUTOMATION_EVIDENCE="$PWD/target/qa/automation-recheck/engine" \
  scripts/cargo.sh test --locked -p luna-engine --test automations_e2e \
  --test attention --test claude_luna_tools --test cross_task_tools
scripts/cargo.sh test --locked -p luna-proto -p luna-doc -p luna-sync --lib
scripts/cargo.sh test --locked -p luna-ui --lib \
  automation_outcomes_follow_their_saved_notification_choice -- --test-threads=1
scripts/cargo.sh test --locked -p luna-ui --lib \
  banner_target_requires_an_exact_l15_environment_and_task -- --test-threads=1
scripts/cargo.sh clippy --locked -p luna-proto -p luna-doc -p luna-sync \
  -p luna-engine -p luna-ui --lib
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
  --example chat-workbench-fixture
LUNA_QA_MCP_ISOLATION=1 \
LUNA_WORKTREES_DIR="$PWD/target/qa/automation-recheck/worktrees" \
  target/debug/examples/chat-workbench-fixture "$PWD/target/qa/automation-recheck/ui"
```

For signed native QA, follow `docs/reference/qa-infrastructure.md` with all four
identity overrides. This run used `LUNA QA Automation Workflow`, bundle identifier
`com.novnski.luna.qa.automation-workflow`, IPC port `59844`, and data under
`target/luna-qa-data/automation-workflow-20261010`. OpenCode used an isolated profile
created by `scripts/qa-opencode-env.sh --no-project-config`, with its wrapper and
wire-log paths supplied to the QA bundle builder. Saved user MCP servers were
excluded by the signed QA isolation setting.

With that app running and those four overrides still set:

```sh
LUNA_AUDIT_NATIVE_BASE="$PWD/target/qa/automation-recheck/native" \
LUNA_QA_AUTOMATION_PROVIDERS=codex,claude-code,opencode \
LUNA_QA_AUTOMATION_TARGETS=existing,newConversation \
  node scripts/qa-automations-live.mjs run
# Quit and reopen that exact QA app gracefully, then:
LUNA_AUDIT_NATIVE_BASE="$PWD/target/qa/automation-recheck/native" \
  node scripts/qa-automations-live.mjs verify-restart
```

## Scope and limits

Automations run while their owning LUNA host is open. An existing chat keeps its
provider/account binding; another provider needs a fresh chat or an explicit
context switch. Custom rules support the documented subset; unsupported fields
(such as COUNT) fail review explicitly. Known checkout identity is pinned, not a
branch or commit. Notification sounds/banners were checked through routing tests;
native QA exercised controls and durable Activity/history, not every macOS focus
and notification-permission combination.

## Handoff and evidence

The QA bundle was signature-verified, its executable hashed in
`final-qa-bundle.json`, then gracefully stopped, unregistered and removed. Its
isolated data and the verification artifacts are preserved. Disposable native
Codex QA chats were archived. Primary LUNA Dev refresh is recorded in
`primary-refresh.log`; its process, IPC listener and strict signature are checked
before handoff. `SHA256SUMS` in the evidence directory identifies the retained
logs, JSON readbacks and screenshots.

Primary LUNA Dev refreshed in **4 seconds**. PID `5537`, the exact bundle
executable, IPC `59777` and strict signature were verified after cleanup
(`primary-handoff.json`). The 99 GiB dev cache exceeded the repository
review threshold: no other LUNA writer/build or open dev-cache file remained;
the verbose Cargo dry-run listed only `target/debug` paths. The scoped dev
profile was then cleaned under the shared guard. Primary/QA data, evidence,
the primary bundle and the shared mbx cache were preserved.
