# Combined Projects native acceptance — prepared, not run

This is a source-only acceptance plan. It grants no build, app launch, desktop
input or provider invocation. The coordinator must first approve the final
combined source and transfer both the packaging and desktop QA slots.

The initial 17-case/check/format/Clippy proof remains tied to runtime source
`04da0cf3` and report `27bec30c`. After separate failure-first reproductions,
combined source `79deab99` passed 34 affected UI cases, the real UUID
MoveProject control, format, focused UI check and Clippy. This includes stale
rendered Run/Retry/Stop and Defaults choice admission in their tested scopes.
The materialization engine companion is still excluded. Do not package until
the coordinator reviews the current main/schema-14 composition and grants
native packaging and desktop QA.
Before packaging, the coordinator must also review/adopt the committed main
composition and schema changes (current Sidebar runtime uses schema 14). Do
not merge main or copy its uncommitted files as part of this preparation.

## Boundaries and ownership

- Use one combined **LUNA QA Defaults** app from this worktree. The Defaults
  worker owns the shared page, defaults and this acceptance record. Commands
  owns command controls, setup and any failure diagnosis in those paths; relay
  exact evidence instead of independently patching its code.
- Limit fixtures to two Git projects A/B, two linked worktrees A1/A2 from A,
  and one plain non-Git folder. Seed two UUID Commands-only tasks in A1/A2
  before provider QA. A separately granted desktop new-task run may add A3,
  and an optional B task after inherit clearing makes four QA chats maximum.
  Use at most three ordinary named commands plus one setup action and one
  transient configuration-only row. Keep all files under this worktree's
  `target/qa/` and isolated dev data directory.
- Use at most two short Codex `gpt-6-luna` completions if current catalog/account
  availability supports them. No other provider needs to run. A different
  provider override can be checked in the new-task draft without sending it.
- One initial refresh and one graceful app restart cover persistence. Extra
  builds, provider runs or fixture expansion require a concrete failure that
  warrants them and the coordinator's next applicable slot.
- Do not switch real accounts/profiles to simulate engine replacement. The
  held-RPC and rendered-action tests cover those precise failure interleavings.
  Do not touch the main app, real project data or another worker's app.

Assigned identity (all four values travel together):

| Field | Value |
| --- | --- |
| App | `LUNA QA Defaults` |
| Bundle ID | `com.novnski.luna.qa.defaults` |
| IPC | `59788` |
| Data | `$PWD/target/luna-qa-data/defaults` |
| Bundle path | `$PWD/target/luna-macos-dev/LUNA QA Defaults.app` |

Use the standard `scripts/run-macos-dev.sh --refresh` with those identity
values and the coordinator-approved build settings. The earlier temporary
10 GiB **check** grants do not authorize carrying that reservation into native
packaging. Fresh accounting, the 170 GiB ceiling, the wrapper and stop-on-75
behavior remain required. No bypass, cache deletion or force quit.

## Bounded acceptance sequence

Record each row as pass, fail, blocked or unverified with exact artifact paths.
A screenshot alone cannot establish persistence, checkout ownership or an
engine outcome; pair UI readback with the corresponding IPC/document receipt.
Use labeled native elements when available and current screenshot evidence
when a control is opaque. Never claim visual observations not actually captured.

| Case | Native action and expected result | Required evidence |
| --- | --- | --- |
| N1 — identity and fixtures | Refresh the approved source, verify the exact bundle process, listener PID on 59788, plist identity/data directory and strict deep codesign. Record the main app PID/bundle fingerprint before and after without operating it. Seed A/B with the existing live helper; create two distinct linked worktrees A1/A2 from A and add the plain-folder fixture. Use `seed-command-tasks <absolute-A1> <absolute-A2>` only after verifying this app. It must mint UUID task/command IDs through local `ExecuteTaskCommand` CreateChat, never write task rows directly, and read back exact branch/cwd/space/device and applied receipts. | Build source/command/duration/log, budget before/after, process/port/plist/signature JSON/text, `WatchSpaces` and two UUID task receipts/readbacks, one app screenshot. |
| N2 — project settings and global fallback | Establish a known global Codex/model/permission choice G through the ordinary new-task picker, then record `composer-defaults.json`. Configure A with a catalog-offered Codex `gpt-6-luna`, supported permission and New worktree. Configure B with a distinct supported provider/model or permission/checkout override. Every group must visibly offer inherit. Switch A → B → A in Projects and verify each saved value; global G must remain unchanged by project edits. | Settings screenshots with selected project, fresh `WatchSpaces` after each save, before/after global-default JSON limited to these non-secret fields. |
| N3 — Commands card and current-frame controls | Before any provider run, add/edit/remove the transient row and verify A's saved commands; B's list stays independent. In the two UUID fixture tasks, run distinct Build A/Build B markers from A1/A2 and verify both cwd/branch/receipt paths stay separate. Probe an exit-7 failure and inspect output. Run the foreground preview server, open its output and preview, Stop it, Retry the captured run, then Stop it again. No command runs during editing/navigation or when a stale rendered control is clicked. These seeded tasks do not prove desktop new-task Defaults selection. | Saved command lists, settings/terminal/preview screenshots, two fixture task IDs/branches/cwds, run receipts/attempt/terminal IDs, markers, observed HTTP response, final stopped process state. |
| N4 — actual new-task path and setup integration | After the separate provider grant, check desktop new-chat affordances against A's default chips without sending duplicates. Configure A's setup action with Wait for setup. Submit one short Codex prompt through the UI. Its new worktree A3 must differ from the seeded A1/A2 checkouts and base A. The initial setup failure must resolve the task's normal receipt visibly before a provider session starts. Repair the readiness marker in A3, retry the setup receipt, then explicitly retry the task. On success expect the exact smoke reply and task config from A; navigation/reuse must not create another setup run. | New-draft/chip screenshot, desktop-created task/Run receipt IDs, A3 cwd/branch, setup run ID/generation/attempt/exit/output, provider-session readback and exact final reply. |
| N5 — blocked defaults and existing-task preservation | Snapshot A task config C. Change A's project defaults and verify C stays identical. Exercise an unavailable pinned model using only the QA project and a fresh expected-default snapshot: the new draft retains that choice and blocks send with actionable text; no task/provider run is created. Restore a real offered choice. On the plain folder, New worktree blocks until Current checkout is chosen. If Full is not confirmed for this QA machine, its project default must lead to the existing confirmation boundary before send. | Before/after task config, negative-action screenshot, `WatchChats`/task receipt delta proving no unintended creation, fresh catalog evidence and restored project values. |
| N6 — restart and explicit inherit clearing | Stop owned commands and save fixture file edits, then gracefully restart only this QA app. Recheck A/B project defaults and Commands config without any automatic command replay. Create a fresh B draft to verify its persisted overrides survived. Explicitly clear B to inherit, inspect the new draft against global G, and optionally send the second Codex smoke to prove persisted task config after clearing. Existing A task C remains unchanged. | New PID with same bundle/data/port, post-restart spaces/actions/receipts, inherited-chip screenshot, B defaults `{}`, global G equality, A config equality and optional second task reply/config. |

Suggested exact smoke prompts (only when provider use is available and granted
within the native slot):

- A: `Reply with exactly PROJECT_DEFAULTS_NATIVE_OK. Do not use tools or change files.`
- B after inherit clearing: `Reply with exactly PROJECT_DEFAULTS_RESTART_OK. Do not use tools or change files.`

The setup-failure contract is explicit: **retry setup, then retry the task**.
Retrying setup alone must not be described as automatically replaying a failed
provider submission. Use the setup **receipt’s Retry** control, not the ordinary
saved-action Run button. A retry retains the captured command and checkout; editing
the saved action changes later new runs, not an existing run's snapshot.
Before Retry, verify the native UI lets the user inspect the captured command.
A captured name alone, or command text visible only in IPC evidence, does not
establish that readability requirement; report any gap to the Commands owner.

## Harmless command fixtures

Use the current task's actual checkout as process cwd. Commands intentionally
use the existing `/bin/sh -c` contract; no path is interpolated into shell text.
These definitions are a reviewable plan, not commands executed by this document.

Setup readiness action, with new-worktree setup and Wait enabled:

```sh
printf 'SETUP_CWD=%s\n' "$PWD"
if [ ! -f .qa-setup-allow ]; then
  printf 'PROJECT_SETUP_BLOCKED\n'
  exit 7
fi
printf 'PROJECT_SETUP_READY\n'
```

After the first failure, create/save only `.qa-setup-allow` in the materialized
QA worktree through the existing file editor or workspace-file API. Do not alter
the captured setup command to manufacture retry success.

Probe action:

```sh
printf 'PROJECT_COMMAND_NATIVE_OK\n'
pwd
```

For one explicit failure probe, edit its saved definition to print
`PROJECT_COMMAND_NATIVE_FAIL` and exit 7, then make a **new** run. Preserve its
failed receipt/output. Restore the harmless successful definition afterward.

The preview server action must use a discovered, already available interpreter
and a verified free localhost port, serve only a dedicated synthetic `preview/`
folder containing `PROJECT_PREVIEW_NATIVE_OK`, and stay in the foreground. Bind
to `127.0.0.1`. Store the exact HTTP URL in its preview field. Observe a real
response before claiming server readiness; opening Preview alone only proves
routing. Stop/Retry must target that owned run/process group, not an arbitrary
PID. Leave no running fixture server at handoff.

## Failure matrix and evidence reuse

| Failure or boundary | Required handling / proof |
| --- | --- |
| Partial identity, busy build lock, guard 75, refused graceful quit, occupied port, failed signing/build/launch | Stop and preserve the current app/data/logs. Report the exact blocker; no retry loop, reservation reduction or force quit. Existing QA infrastructure tests remain the failure proof for destructive wrapper paths. |
| Missing/foreign project, invalid defaults, stale expected-default write | Existing Defaults engine tests establish rejection and row preservation. Native saves must still use current authoritative readback. Do not create real remote accounts/devices merely to repeat these cases. |
| Unavailable provider/model, unsupported permission or non-Git worktree | Capture the actual current catalog/capability/folder state first. The UI must retain the configured choice and block or require the supported user correction; no silent provider/model/checkout fallback. |
| Stale Commands settings editor | If native CAS coverage is needed, use a second local IPC client against the same QA project to change one harmless action while the UI draft is retained. The UI save must conflict without losing its draft; explicit reload then deliberate save must recover. Do not patch around a conflict or replace real project actions. |
| Failed/canceled setup with Wait enabled | Failed receipt and output must be inspectable; the agent submission is not silently accepted. Retry setup explicitly, then retry the task. Reuse/import/navigation and editing must not re-run setup. Backend generation/durability proof is retained, not re-created with destructive native fixtures. |
| Old engine results or old rendered Run/Retry/Stop input | The accepted held-RPC evidence covers result isolation; Commands' newly owned actual-rendered admission cases are a separate mandatory prerequisite. Native normal-path success cannot substitute for those interleavings. Preserve their distinct baseline/fixed source and results. |
| Stop/Retry after navigation, restart, or changed task ownership | Use only current-frame controls and captured run scope. Existing backend attempt/process-group/ownership tests remain authoritative for restored or stale process cases; never signal a recorded PID manually. |
| Provider/desktop/tool unavailable | Record only the checks actually performed and mark the rest blocked/unverified. Do not call a source inspection, IPC-only check or screenshot an end-to-end native/provider pass. |

## Artifact and completion record

Use a fresh directory such as
`target/qa/projects-native/<approved-source>-<run-id>/`. Record approved source,
actual executed source/dirty state, source-test prerequisites, app identity,
exact commands/exits/durations, UI observations/screenshots, every relevant
project/task/action/run/attempt/receipt ID and before/after readbacks.

`scripts/qa-project-defaults-live.mjs` is already restricted to port 59788.
Its `seed` and `read` output paths are fixed; immediately copy each readback into
its uniquely named stage artifact before another read overwrites the helper's
latest file. `clear` is an explicit QA-project mutation, not cleanup to run
without checking the current expected values. Do not point this helper at main.

A final native result must state the exact process executable/PID, listening
port ownership, strict signature result, build duration, restart readback,
case statuses and limitations. Keep the packaged QA app, its data, fixtures
that it still references, and all evidence available for coordinator review.
Remove only disposable task-created fixtures no longer referenced by a live
app/task. Do not merge to main, push, deploy or archive this worktree as part of
acceptance.
