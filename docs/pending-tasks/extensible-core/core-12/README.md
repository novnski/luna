# CORE-12: native UI contributions and view models

Baseline: `11b97743` (14 October 2026; `85332980` on top changes only the
queue files). Live status belongs to `pending_tasks.py`. This file is the
contract, the failure cases (written before the code), the criteria mapping
and the evidence plan.

## Rebased against current main

What already existed and is reused:
- **CORE-00 decision:** bounded, data-only native contribution descriptions
  (stable namespaced id, kind, label/icon, location, host-issued action,
  required capability); the host constructs every GPUI element and owns
  selection, drafts, focus, AX, lifecycle and protected surfaces; nothing of
  an extension runs synchronously in paint.
- **CORE-01:** `ContributionId` and the operation catalog (a new row is
  `localOnly` until it names a scope).
- **CORE-06:** manifests already accept the `command`, `settingsSection`,
  `sidebarSection` and `panel` contribution kinds, namespaced under the
  package id (`luna.*` reserved); hash-pinned package files; explicit
  enablement per profile.
- **CORE-07:** `StartExtensionInstance` (re-verification, exact grants, the
  task target rule: a local task with a checkout), `CallExtensionInstance`
  (deadlines, statuses, never replayed), `StopExtensionInstance`, the trust
  table. **Native workers are not OS-sandboxed**; nothing here changes that.
- **CORE-08:** the pattern of a hash-pinned declaration file read without
  running code (`luna-provider.json`) and reconciliation after every
  enablement change; the existing owner (there the picker, here the shell)
  serves extension entries next to its own.
- **CORE-19:** update, rollback, removal and safe mode call the same
  change hooks; request ids are reconciled, never replayed; Settings →
  Extensions is the protected recovery route.

What was missing:
1. Nothing placed an enabled package's UI contributions anywhere; the kinds
   were accepted and ignored.
2. The shell's commands, settings sections and sidebar pages were closed
   enums with no shared descriptor or registry (source audit F07), so a
   contribution had no way to join them, and no saved layout could name one.
3. No declarative view vocabulary, bounds, view/action operations, stale
   callback rule or fallback for a removed contribution existed.

## Contract

Types: `crates/proto/src/ui_contributions.rs`. The extension API is now
**1.3** (adds `luna-ui.json`); a package using it sets `api.minMinor` to 3.

### Declaration (`luna-ui.json`, version 1)

A listed, hash-pinned package file, read and checked against the manifest's
SHA-256 before it is parsed; no code runs to read it. Unknown fields are
refused everywhere.

| Field | Rule |
| --- | --- |
| `id` | A manifest contribution of the matching kind. Every manifest UI contribution needs exactly one declaration, and the file declares nothing else. |
| `placement` | `command` (palette entry), `settingsSection`, `sidebarSection` (sidebar navigation page) or `panel` (right-rail place beside one task). Equals the manifest kind. |
| `title` | One line, at most 80 characters, never a protected control's name (`UI_PROTECTED_TITLES`: Extensions, Safe mode, Turn off safe mode, Permissions, Permission mode, Allow, Deny, Approve, Send, Stop). |
| `icon` | Optional, from `UI_ICONS` (widget, list, checklist, notebook, info, star, bolt, clipboard). |
| `order` | Optional 0 to 1000 (default 500), among extension entries only. |
| `context` | `none` (default) or `task`. A `panel` is always `task`. |
| `view` | `{"static": {"nodes": [...]}}` or `{"worker": {"method": "..."}}`. Required for every placement but `command`, which has none. |
| `actions` | `{id, title, run}`, ids unique. A `command` has exactly one (run when chosen); others up to 16, named by their buttons and items. `run` is `{"host": ...}` or `{"worker": {"method": "..."}}`. |

Host actions (the closed set the shell performs with its own navigation):
`"newTask"`, `{"openSettings": {"section": <built-in key>}}` and
`{"openContribution": {"contributionId": <a view of the same package>}}`.
Worker methods are 1 to 64 bytes of `[A-Za-z0-9._-]`; the `luna.` and
`provider.` prefixes are reserved. A `data` package can declare no worker
view or action.

### Views (bounded view models)

A flat list of at most 128 nodes and 32 KiB serialized: `heading`, `text`
(with `tone`), `field` (label and value), `item` (id, title, optional detail,
badge and action), `button` (an action and a label, `primary` or
`secondary`) and `divider`. Lines are bounded (80, 200 or 2,000 characters)
and contain no control characters; item ids are unique; every action named
exists. A worker view (and an action's replacement view) is validated by the
engine with exactly these rules before the shell sees it; the shell renders
at most the bounds again.

### Placement, ordering and protected host UI

- The shell reads one registry (`crates/ui/src/contributions.rs`). LUNA's
  own palette commands, settings sections and sidebar pages are registered
  there too, with the same descriptor (`luna.shell/...` ids, placement,
  title, icon, context), and are listed first in their shipped order.
  Extension entries follow and can never replace, reorder or hide them.
- Settings → Extensions (with Install, Disable, Remove, Safe mode) is a
  protected host entry. Permission and approval controls and the composer's
  Send and Stop are host UI no contribution can reach: there is no placement
  for them and no host action that touches them.
- Every extension surface names its extension: rows and palette entries
  carry `<title>, from <extension>` as their accessible name (and the
  palette shows the extension's name beside the title); pages and panels
  carry a host-drawn attribution line (`From <name> vX · data only | portable
  code | native code, not sandboxed`).

### Identity, lifetime and stale callbacks

- The registry has no state of its own in the engine: each read derives it
  from the installed index, this profile's grants and safe mode, so install
  never places anything, and disable, update, rollback, removal, restart and
  safe mode can never leave a stale entry. Every enablement change bumps a
  watch; `WatchUiContributions` publishes a new snapshot when its revision
  changes.
- Every view request and action names the exact `packageHash` it was
  rendered from. Any other (or no) provider of the id is `stale`: nothing
  starts and nothing runs. A `task` contribution names its exact task, which
  the server binds (CORE-07 rule); the outcome echoes it, and a worker
  receives it only as its server-bound target.
- The shell caches views by (contribution, package hash, task). A reply
  lands only in the slot that asked, and only while that slot still waits
  for that request; a package change drops every view of the old hash. An
  action carries the task its control was shown for, its outcome applies to
  that task only (its message names the task when another is now shown), and
  a second press waits for the first.
- A worker view or action starts one supervised instance of the exact
  package (and task), calls one required method with a deadline (5 s for a
  view, 15 s for an action) and stops it. Its request id goes to
  `CallExtensionInstance`, so an unknown outcome is reconciled, never resent.

### Fallback

| Gone (disabled, updated away, removed, safe mode) or unknown | Falls back to |
| --- | --- |
| An open contributed sidebar page | Tasks (`Route::Chat`), with a notice |
| An open contributed settings section | Settings → Extensions (the recovery route) |
| A shown contributed panel | The task's Overview |
| A saved layout (`lastSidebarPage: "contribution:<id>"`) | Restored only once the first snapshot lists it; otherwise LUNA stays on tasks and the saved value is replaced |
| A palette command or rail place | Not listed |

Nothing of this moves the selected task or touches the composer.

### Owner methods

All four are catalog rows with `local` target and `localOnly` authority,
served only by the engine's local service (the in-process app and owner
IPC); a remote session and an extension worker never reach them.

| Method | Kind | Request → reply |
| --- | --- | --- |
| `ListUiContributions` | query | `{}` → `UiContributionsSnapshot {revision, safeMode, contributions, refused}` |
| `WatchUiContributions` | watch | `{}` → the snapshot, then one per revision change |
| `GetUiContributionView` | query | `{contributionId, packageHash, taskId?}` → `UiViewOutcome {status, view?, message?, taskId}` |
| `InvokeUiAction` | command | `{contributionId, packageHash, actionId, taskId?, requestId?}` → `UiActionOutcome {status, host?, view?, message?, taskId}` |

`contributions` is in a fixed order: by placement (`command`,
`settingsSection`, `sidebarSection`, `panel`), then each entry's `order`,
then id; `refused` is by contribution id. The shell places extension entries
in that order after its own.

Statuses: `ok`, `stale` (another or no package provides the id: refresh),
`refused` (safe mode, unknown action, missing, unknown or wrong task) and
`failed` (the worker refused, timed out, exited or returned something
invalid). Unknown request fields (a forged `confirmed: true`) are `bad
params`.

## Failure contract (written before the code)

| ID | Failure | Defence | Owner test |
| --- | --- | --- | --- |
| C1 | Installing places UI, or an entry outlives disable | Derived from grants on every read; install writes no grant | engine `contributions_follow_enablement_restart_and_safe_mode` |
| C2 | A restart loses, duplicates or changes entries | No engine-side registry state; same revision after reopen | same |
| C3 | Safe mode leaves extension views or actions live | Empty list, every id refused; views/actions `refused` | same |
| C4 | A missing, malformed, unknown-field, wrong-version, tampered or stray declaration is placed | Hash pin, strict parse, version, exact id set | engine `invalid_declarations_are_refused_with_their_reason` |
| C5 | A title spoofs a protected control; a data package declares code; a panel has no task; undeclared actions, unknown icons or host targets pass | Per-contribution checks; siblings stay placed | same |
| C6 | A callback rendered from an old package runs after an update (stale callback) | Exact package hash, else `stale` before any start | engine `stale_and_misdirected_calls_run_nothing` |
| C7 | A task request runs without, for an unknown, or for another task; a non-task command takes one | Context rule; server-bound target; echoed task | same |
| C8 | A crashed action is replayed under its request id; a worker outlives its call | CORE-19 ledger; stop after every call; OS readback | same |
| C9 | An oversized or undeclared-action worker view is shown; a hung worker stalls the caller | Engine-side view bounds; 5 s view deadline | engine `worker_views_are_bounded_before_the_shell_sees_them` |
| U1 | An extension entry precedes, displaces or hides a host entry (Extensions above all) | Host entries first, unchanged; protected flag | UI `extension_entries_join_after_the_host_and_leave_without_moving_task_or_draft` |
| U2 | A contributed row is not painted, not reachable by Tab, or does nothing on Space; its view is not the declared one; its host action does not navigate | Host row builder (role, tab stop, focus ring, keyboard click); host renderer | same |
| U3 | Removing a package leaves a dead page, section or panel, or moves the selected task or the draft | Route/rail fallback; nothing touches selection or composer | same |
| U4 | An unknown saved id breaks startup or leaves a blank page; a known one is not restored; the saved layout is overwritten before the first snapshot | Deferred restore; boot route kept meanwhile | UI `saved_layouts_and_late_replies_fall_back_to_known_good` |
| U5 | A late reply for task A replaces the panel shown for task B (stale callback targeting the new tab) | Views keyed by exact task; reply fenced by request | same |
| U7 | The sidebar (a cached view) misses a snapshot: the row is missing after enable, or left behind after safe mode or disable, until something else re-renders it (found in native QA) | A registry change invalidates the sidebar and refreshes every window | UI `a_watch_snapshot_alone_adds_and_removes_the_sidebar_row` |
| U6 | Plugin work runs synchronously in a frame, or updates are unbounded | Views fetched only on open, Refresh or an action, off the render thread; bounded trees | by construction; native QA frame check |

Independent readback in the engine suite: the declaration bytes the test
wrote, the grants file, the worker's own journal of each call and the target
the server bound in its hello, its side-effect file, the CORE-19 ledger and
process existence from the OS. The UI suite drives a real engine through a
separate client for every package change and reads the shell's own state,
painted bounds and composer. Package hashes are recomputed from the
published rule in both.

These risks are **not tested** automatically:
- Accessibility names and the macOS AX tree (GPUI's test platform drops AX
  updates): native QA.
- Pixels, theme/contrast, narrow windows and reduced motion of contributed
  surfaces: native QA screenshots (CORE-13 owns the full custom-layout
  acceptance).
- Portable (Wasm) worker views: the same supervisor call, not exercised.
- Non-macOS rendering; the engine suite is `#![cfg(unix)]`.

## Criteria mapping

- **contract:** this file and `crates/proto/src/ui_contributions.rs` (ids,
  placement, context, lifetime, fallback, protected host UI, bounded views);
  no plugin GPUI entity exists anywhere in the path, and render reads only
  cached data.
- **delivery:**
  - engine: `crates/engine/src/extensions/ui.rs` (registry, validation, views,
    actions, watch), `crates/engine/src/rpc/extension_ui.rs`, and the four
    catalog rows;
  - shell: `crates/ui/src/contributions.rs` (one registry for host and
    extension entries) and `crates/ui/src/shell/contributions.rs` (watch,
    view cache, actions, host renderer, fallbacks). The palette's commands,
    Settings' sections and the sidebar's pages are now read from that
    registry; extension panels join the right rail;
  - example: `examples/extensions/ui-notes/` (a data package with all four
    placements and a native worker package with a live task panel).
- **verification:** E12 is the four engine tests and the three UI tests above.
- **handoff:** the orchestrator's build, tests, lint, review, native QA,
  LUNA Dev refresh and delivery.

## Commands (orchestrator)

```sh
python3 scripts/build-budget.py --status
scripts/cargo.sh check --locked -p luna-proto -p luna-rpc -p luna-engine -p luna-ui
scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract   # operations.json (+4 rows; already added by hand)
scripts/cargo.sh test --locked -p luna-rpc --features schema --test contract
scripts/cargo.sh test --locked -p luna-rpc --lib remote_auth::tests::policy_inventory_is_complete_and_unknown_fails_closed
EVIDENCE="$PWD/target/qa/core12/$(date -u +%Y%m%dT%H%M%SZ)"
LUNA_CORE12_EVIDENCE_DIR="$EVIDENCE" CARGO_INCREMENTAL=0 \
  scripts/cargo.sh test --locked -p luna-engine --test extension_ui -- --nocapture
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine \
  --test extension_packages --test extension_runtime --test extension_lifecycle --test extension_providers
LUNA_CORE12_EVIDENCE_DIR="$EVIDENCE" scripts/cargo.sh test --locked -p luna-ui --lib \
  shell::contributions_tests -- --test-threads=1
scripts/cargo.sh test --locked -p luna-ui --lib shell::command_palette -- --test-threads=1
scripts/cargo.sh test --locked -p luna-ui --lib settings_entry_and_fixture_routes -- --test-threads=1
scripts/cargo.sh test --locked -p luna-ui --lib lifecycle_states_and_actions_follow_the_engine -- --test-threads=1
scripts/cargo.sh check --locked -p luna-ui --features appshots-fixture --examples
scripts/cargo.sh clippy --locked -p luna-proto -p luna-rpc -p luna-engine -p luna-ui --all-targets
python3 -m py_compile examples/extensions/ui-notes/pack.py examples/extensions/ui-notes/tasknotes/bin/worker
(cd "$EVIDENCE" && shasum -a 256 *.json > SHA256SUMS)
```

The engine suite needs `python3` on `PATH` (as the CORE-07/08/19 suites do)
and is added to the hosted UI workflow next to CORE-08's. The UI tests use
only a data package and run in the existing `luna-ui --lib` job.

## Native QA (a QA app per the QA guide, local data only)

Pack the examples (each line prints a directory and its package hash):

```sh
python3 examples/extensions/ui-notes/pack.py /tmp/core12-qa
python3 examples/extensions/ui-notes/pack.py /tmp/core12-qa-next 1.0.1
```

Owner IPC helper, as in CORE-19, with the QA app's `LUNA_DEV_IPC_PORT`:

```sh
ipc() { node -e '
const [m, p] = process.argv.slice(1); const ws = new WebSocket("ws://127.0.0.1:" + process.env.LUNA_DEV_IPC_PORT);
ws.onopen = () => ws.send(JSON.stringify({id: 1, method: m, params: JSON.parse(p || "{}")}) + "\n");
ws.onmessage = ({data}) => { console.log(JSON.stringify(JSON.parse(String(data).split("\n")[0]), null, 2)); ws.close(); };' "$1" "$2"; }
```

1. **Join, no restart.** Settings → Extensions → Install…
   `/tmp/core12-qa/notes` → Review and enable → Enable. Without restarting:
   - the sidebar shows **Notes** below Board;
   - Settings lists **Notes** after Archived sessions, in its own group, with
     Extensions still in place above it;
   - ⌘K lists **Open notes appearance** after the built-in commands, with
     "CORE-12 notes" beside it; choosing it opens Settings → Appearance;
   - a task's right sidebar has a **Task notes** place after Browser, below a
     line.
   `ipc ListUiContributions` lists the four ids with the package hash.
2. **Keyboard and AX.** From the sidebar, Tab through New thread,
   Automations, Board to Notes; Space opens the page. Tab reaches "Write the
   CORE-12 QA notes" and both buttons; Enter on **Open notes settings** opens
   Settings → Notes, whose **Show recent notes** returns. With Accessibility
   Inspector, record the names: `Notes, from CORE-12 notes` (sidebar),
   `Notes, from CORE-12 notes settings` (Settings),
   `Open notes appearance, from CORE-12 notes` (palette),
   `Task notes, from CORE-12 notes` (rail).
3. **Task and draft survive removal.** Open a task, type a draft (do not
   send) and open its **Task notes** panel, then open the Notes page. While
   the page is shown, disable the package over owner IPC (Settings would
   navigate away from the page):
   `ipc SetExtensionEnabled '{"extensionId":"core12.qa/notes","packageHash":"<hash>","enabled":false,"grants":[]}'`.
   The page closes to the task with the notice "That extension page is no
   longer available"; the same task is selected, its draft is intact, the
   rail place is gone and the right sidebar shows the Overview.
4. **Saved layout.** Re-enable, open the Notes page, quit gracefully and
   check `ui-settings.json`: `"lastSidebarPage": "contribution:core12.qa/notes.page"`.
   Relaunch: the Notes page reopens once the app connects. Quit, change the
   value to `"contribution:core12.qa/missing.page"` and relaunch: the app
   opens on tasks (never a blank page) and, once connected, the value is
   replaced.
5. **Safe mode.** Settings → Extensions → Safe mode: every extension entry
   (sidebar row, Settings section, palette command, rail place) disappears
   and an open Notes section falls back to Settings → Extensions; the
   Extensions page, its Safe mode banner and Turn off stay reachable. Turn
   off safe mode: the entries return.
6. **Live panel (native worker).** Install `/tmp/core12-qa/tasknotes` →
   Review and enable (the native-code warning; native code locked on). Open
   task A's right sidebar → **Live task notes**: Task is A's id, a worker pid
   and time. Ping: the notice "CORE-12 live task notes: Pong for task A…" and
   the panel updates. Switch to task B: the panel shows B's id. After each
   view or ping, `ps -p <pid>` finds no worker. ⌘K → **Ping this task's
   notes** pings the shown task.
7. **Stale callback after an update.** With task A's live panel shown,
   install `/tmp/core12-qa-next/tasknotes` and **Update to v1.0.1** (stop
   running work). The panel refetches and shows `v1.0.1`. Then
   `ipc InvokeUiAction '{"contributionId":"core12.qa/tasknotes.panel","packageHash":"<1.0.0 hash>","actionId":"ping","taskId":"<task A id>"}'`
   answers `"status": "stale"` and starts no worker
   (`ipc ListExtensionInstances` shows no new instance).
8. **Frames.** With the Notes page and a live panel open, scroll and resize
   for a few seconds; idle CPU returns to its baseline (no view requests
   unless opened, refreshed or acted on). Record `scripts/resource-profile.mjs`
   if a regression is suspected.
9. Capture screenshots and AX names for steps 1, 2, 5 and 6, and hash them
   with the logs into the evidence folder.

## Limitations and hand-offs

- **Selected built-ins only.** The palette's ordinary commands, Settings'
  sections and the sidebar's pages read the registry. Workbench and Board
  palette entries, keybindings and menus, and the right rail's own places
  are still the shell's own lists (extension panels join after them).
  Keybinding and menu contributions are not offered yet.
- **No settings values.** A `settingsSection` shows a view and actions; the
  reserved settings-value scope (`{store root}/extensions/settings/<id>`)
  and settings schemas remain for a later ticket.
- **No new grant.** Enabling a package (the explicit review) is the consent
  for its UI entries; there is no separate `ui` capability. A worker view or
  action runs with exactly the grants the package was enabled with.
- **Host data in views.** Views carry only the package's own data (and a
  worker's, via its grants). Binding to host-owned task/project data for an
  alternate sidebar is CORE-13's, on this boundary.
- **Refusals are an owner read.** `ListUiContributions.refused` names each
  refused contribution and why; Settings → Extensions does not show them
  yet.
- **Per-call workers.** Each worker view or action starts and stops one
  instance (process start cost per call; no long-lived UI worker).
- **Attribution, not proof.** A protected-title list and attribution keep an
  entry from passing as a host control; they do not judge content.
- **Native workers** remain user-level code without an OS sandbox (CORE-07).
- **Later tickets:** CORE-13 (theme and alternate sidebar examples on this
  registry), CORE-14 (durable content renderers), CORE-16/17 (browser and
  diff feature packages), AW-01..08 (settings and workspace items).
