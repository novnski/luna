# IOS-02: Adaptive phone and iPad shell, organization and appearance

Source before the change: `729cc458` (14 October 2026; IOS-00 and RM-03
included, no iOS change since `11b97743`). Live status belongs to
`pending_tasks.py`. This is the contract and the evidence plan; the
orchestrator records results here once it has run them. The failure cases
below were written before the code.

## Baseline (rebased on current code)

- **IOS-00:** the owned core (`crates/mobile`, UniFFI `luna_core`) and
  `RemoteHostSession`; bundle `com.novnski.luna.ios`, iOS 26 floor, paired-Mac
  metadata `luna.paired-macs.v1` plus one Keychain token per Mac.
- **RM-03:** the host's `WatchRemoteTasks` / `WatchRemoteTask` projections,
  `RemoteSession::following` (reconnect, suspend/resume, network, a bounded
  host-keyed cache in `Application Support/LunaRemote/tasks-<id>.json`), and
  a minimal list plus history inside the Paired Macs **sheet**
  (`RemoteTasksView.swift`). Freshness is `live`, `stale` or `cached`.
- **App icon:** `Luna/AppIcon.icon` already has light, dark and tinted
  renditions chosen by the Home Screen; no app code is involved.

**Gaps found:**
1. No shell for direct V1. Without an edge account the app opens on sign-in;
   a paired Mac's tasks exist only inside a sheet, as a flat list.
2. No adaptive navigation: no iPad split, no sidebar, no task detail beside
   the list; rotation or a relaunch keeps nothing.
3. Cold start never reconnects: `PairedMacsModel.selectedId` is memory only,
   so after a relaunch nothing is shown until the user opens the sheet and
   taps Connect.
4. A failed RM-02 verification leaves RM-03's line at "Opening live
   connection…" forever (the runtime is never started), which is exactly
   the permanent opening state the spec names.
5. No organization: no filters, project scope, search, pinned section or
   archived view; `pinned`, `unseen` and `archived` arrive and are ignored.
6. No drafts for a Mac's task, and no restoration of any route.
7. Appearance is forced dark twice (`.preferredColorScheme(.dark)` and
   `UIUserInterfaceStyle = Dark`); `Theme` is dark-only; there is no
   settings screen.
8. Zeron's fixed bug (a failed open substituting a fixture conversation) is
   **not** present in LUNA's direct path: RM-03's sheet shows "Waiting for
   this task from the Mac…" and never demo data. Demo data is reachable only
   through the `-demo` launch argument and the edge `HomeView`.

## Ownership contract

| State | Owner | Lifetime | Rule |
| --- | --- | --- | --- |
| Projects, tasks, history, status, pinned, unseen, archived | The Mac (RM-03 projections through `PairedMacsModel`) | Host | Read only. Shown with its freshness. Never copied into shell state; never inferred for rows the Mac did not list. |
| Selected Mac | `PairedMacsModel.selectedId` (app-wide) | Memory, restored from the scene record | One runtime follows one Mac. A Mac that must pair again is selected after the previous runtime closes, so another Mac's rows never show under its name (`projectedHostId` guard). |
| Host/profile identity | RM-02 metadata and the projection's `deviceId` | Saved Mac | Always visible: `<Mac name> · host <id8>` above the list; Settings shows host, route, live state and expiry. |
| Scene route: project, filter, search, open task | `ShellScene` per window, persisted by `ShellRestoration` (`luna.shell.v1`) | Per scene, across rotation, split changes and relaunch | Stable host ids, never indexes. Saved on every change (no debounce). A scene the system did not restore takes the most recently changed record. At most 8 scenes. |
| Draft | `DraftBook` (`LunaRemote/drafts-<saved Mac id>.json`) | Per Mac and task, until sent (IOS-03), discarded or the Mac is forgotten | The user's text, never a Mac record. Saved as typed; kept when the task is removed; file-protected and excluded from backup like the cache; 100 drafts per Mac, 64 KiB each. |
| Appearance and density | `AppearancePreferences` (`luna.appearance.v1`, `luna.list-density.v1`) | Device | Local presentation, not authority. Values are the desktop's `system`, `light`, `dark`. |

**Route recovery** (what a restored or selected route shows, in order):
1. Mac no longer paired: the route is dropped with a notice ("The Mac this
   window had open is no longer paired with this phone.").
2. Mac must pair again: "Pair <Mac> again" with Open Paired Macs.
3. The Mac's detail for the task says `removed`: "This task is no longer on
   <Mac>", nothing stale in its place, the kept draft with Copy and Discard,
   and Back to tasks. A restored id the Mac never listed is answered the same
   way by the host (RM-03 answers `removed` before opening a document).
4. A detail exists (live, stale or cached): it is shown with its freshness;
   Retry appears whenever it is not live and a retry can help.
5. No detail yet and connected or connecting: "Loading this task from
   <Mac>…", with the list row's title if listed; after 15 s "Still waiting…"
   with Retry.
6. No detail and not connected: "Not available on this phone" with the reason
   and Retry; the draft stays editable.

**Connection states** (`MacLink`, from RM-02's phase and RM-03's runtime
state) all settle: `connecting` ends in a list, `unreachable` (verification
failed or the connection ended; the reason and Retry), `interrupted` (the
runtime retries by itself), `pairAgain` or `live`. A stale list's freshness
line uses this word, never RM-03's "Opening live connection…".

**Organization:** filters All (not archived), Needs you (needs approval or
input, or failed), Working, Unread, Archived; a project scope (sidebar on
iPad, project menu everywhere); search over title, project, preview and
provider. All of it applies only to the rows the Mac listed, in the Mac's own
order (pinned first, then recent); "N older tasks are on the Mac and not
listed here" when bounded. Empty results say whether filters hide rows.

**Supported list actions:** Open and Copy Title. The current host advertises
no task control, so the list says "Read-only: this Mac does not accept task
changes from the phone yet." No Archive, Pin or Mark read control is shown.

**Appearance migration** (`AppearanceMigration.resolve`, written once):
- a stored valid value is kept;
- no or an unreadable value on an install an earlier build already ran (any
  of `edgeURL`, `luna.paired-macs.v1`, `deviceId`, `homeSpaceFilter`,
  `authMode`, `newSessionHarness`) becomes `dark`, the look it had;
- the same on a fresh install becomes `system`, the desktop default;
- a downgrade ignores the key and is dark again. `LunaApp.init` runs it on
  every launch, paired or not, before any view or `restore()` writes this
  run's preferences, and flushes its one write. (It must not wait for the
  `AppearancePreferences` state object: SwiftUI creates an `@Observable`
  `@State` value lazily.)

The shell follows the choice; sign-in and the edge-backed screens keep the
dark design they were built for (their transcript and composer forced dark
before this change and are IOS-01/IOS-03's). `Theme` now resolves every
color from the trait collection, with the desktop `luna-light` seeds for
light, so a forced-dark screen paints exactly the shipped dark values.

## What changed

**New (owned, `ios/shell`, `ios/settings`, `ios/threads`):**
- `Luna/Shell/MacShellView.swift`: the three-column `NavigationSplitView`
  (balanced; on iPad the list beside the task in every orientation with the
  sidebar one toggle away; one stack on iPhone and a narrow iPad split),
  sidebar (Macs, projects, Settings), task list (identity, freshness, problem and Retry,
  read-only note, wrapping filter chips, Pinned and Tasks sections), the
  connection states, row VoiceOver labels and the hardware-keyboard commands
  (Command-F, Command-], Command-[, Command-R, Command-,).
- `Luna/Shell/ShellModel.swift`: `ShellScene`, `ShellOrganizer`, `MacLink`,
  `TaskStatusStyle`.
- `Luna/Shell/ShellRestoration.swift`: `TaskFilter`, `ShellSceneRecord`,
  `ShellRestoration` (bounded, validated).
- `Luna/Shell/ShellDrafts.swift`: `DraftBook`.
- `Luna/Shell/ShellKeyCommands.swift`: Command-] and Command-[ as UIKit key
  commands on a responder that claims first-responder status only when it
  appears or the app becomes active, with no responder and no text input
  editing (a cold relaunch); it yields when editing begins or Command-F asks
  for search. Command-F, Command-R and Command-, are SwiftUI shortcuts.
- `Luna/Threads/ShellTaskDetailView.swift`: `TaskRouteState` and the detail
  (history, freshness, removed, loading, unavailable, draft editor).
- `Luna/Settings/AppearancePreferences.swift`, `ShellSettingsView.swift`:
  appearance, density, iOS text size and Reduce Motion readback, the Mac's
  identity, Manage paired Macs, core identity, and Log in to LUNA sync when
  the bundle has an edge configuration.

**Edited (owned area):**
- `Luna/App/LunaApp.swift`: the preference owner, created first; the shell
  when signed out with a paired Mac (after restore, so an edge account never
  flashes it); the color scheme policy above.
- `Luna/Theme/Theme.swift`: adaptive colors, `whiteAlpha` black on light.
- `Luna/Info.plist`: `UIUserInterfaceStyle = Dark` removed (it overrode any
  preference).
- `Luna/Remote/PairedMacsModel.swift` (RM-03's owner, additive):
  `liveState`, `projectedHostId`, `projectionStamp`, `taskDetail(_:)`,
  `follow(_:)` (bounded to four, re-applied when the runtime is rebuilt), and
  Forget also removes the Mac's drafts.
- `Luna/Remote/RemoteTasksView.swift`: the entry list extracted as
  `RemoteEntriesList`, shared by the sheet and the shell.
- `apps/ios/README.md`: a Paired-Mac shell section.

**Shared hunks (outside the iOS area):**
- `crates/engine/tests/remote_access.rs`: one additive block in
  `mod gateway` (constants, the `#[ignore]`d native campaign
  `ios02_native_shell_serves_the_phone_and_deletes_stops_and_restarts_on_cue`
  and `seed_ios02_tasks`), reusing the module's helpers.
- `scripts/ios/qa-native-session-sim.py`: the `ios02` campaign (the UI test
  runner's container, `--include-ignored`, and `lunaRemoteFiles` in the
  summary).

No `crates/mobile`, `luna_core.swift` or project-file change: new Swift files
join through the synchronized folders.

## Failure contract (before code) and owners

Owners:
- **Native E2E (P03):** `LunaUITests/AdaptiveShellUITests` against the real
  gateway campaign above (iPhone and iPad Simulator), coordinated by
  `scripts/ios/qa-native-session-sim.py --campaign ios02`.
- **Tables:** `LunaTests/ShellPreferencesTests` (the migration rule and
  restored-route validation, both pure functions the app calls).
- **Native QA:** the manual steps below (split/multitasking, VoiceOver,
  Reduce Motion, a QA LUNA app as the host) and the data-continuity QA.

| # | Failure | Owner and proof |
| --- | --- | --- |
| F1 | Fake, demo or cached-as-live data on a failed, missing or offline route | No shell path reads `DemoDataset`; every list and detail carries the Mac's freshness. E2E P03.9/P03.10: a stopped Mac shows `Stale`, a relaunch offline `Cached`, never `Live`; P03.7: nothing stands in for a removed task. |
| F2 | A permanent opening state | `MacLink` maps every RM-02 failure phase to `unreachable` with Retry; detail loading turns to "Still waiting" with Retry after 15 s; a refused projection shows its issue. E2E P03.10: Retry appears and a failed retry settles with Retry again. |
| F3 | A task deleted while open lingers as current or opens another task | Host `removed` → removed state; the row leaves the list; the selection stays until the user leaves. E2E P03.7. |
| F4 | An invalid restored route (deleted while the app was closed, a forgotten Mac, a corrupt record) shows content, spins or crashes | Host answer for the restored id (E2E P03.8); forgotten Mac → notice and the route dropped (review; needs two Macs); corrupt or foreign record → `ShellRestoration.validated` (table). |
| F5 | Rotation, split collapse or expand opens the wrong task or clears it | Selection is a host id in `ShellScene`, persisted; the list is selected by id, never index. E2E P03.5 (rotation, iPad list beside task); native QA split steps. |
| F6 | A draft is lost on rotation, split, navigation, deletion or relaunch | `DraftBook` saves as typed, keyed by Mac and task; kept on removal with Copy/Discard; removed only by Discard or Forget. E2E P03.5–P03.8, P03.10, P03.13. |
| F7 | Unread is faked or lost | Unread comes only from the Mac's row; the phone never marks seen. E2E P03.4: still "Unread" after opening. |
| F8 | Another Mac's tasks, or the identity hidden | `projectedHostId` guard; scene state reset when the Mac changes; identity line and Settings. E2E P03.2 (`host <id8>` from the engine). |
| F9 | Filters lie (hidden rows without saying so, wrong counts) | `ShellOrganizer` over listed rows only, counts on every chip, explicit empty and bounded-list texts, Clear filters. E2E P03.3. |
| F10 | Unsupported actions offered (archive, pin, mark read) | Only Open and Copy Title; read-only note from host capabilities. Review; `shell.read-only` in screenshots. |
| F11 | The theme migration flips upgraders to light, ignores iOS on fresh installs, or crashes on an unknown value | `AppearanceMigration.resolve` (table) and the upgrade/fresh/downgrade QA below. |
| F12 | Light theme or large text hides controls or text | `luna-light` palette, hierarchical styles on selected rows, wrapping filters, AX layouts that stack. E2E P03.12 (Light/Dark/System applied and kept), P03.13 (largest AX size: controls hittable and the native accessibility audit on the shell's own elements). |
| F13 | No keyboard path (search, next task, reconnect, settings), including Command-] right after a relaunch, when iOS has no first responder; or a keyboard path that keeps the keyboard from the search field | SwiftUI shortcuts for Command-F/,/R (Command-F sets the search focus) and on the task column's visible controls (Command-[ / Command-] / Command-R / Command-, in compact), plus `ShellKeyCommands` for Command-[ / Command-] on a cold relaunch (it never claims while text edits and yields to Command-F). Command-F, Command-], Command-R, Command-, in E2E P03.3/P03.7 (right after a relaunch)/P03.11/P03.12; arrow-key list selection is native QA. |
| F14 | VoiceOver reads rows or states unlabeled, freshness not first | Explicit row labels (title, project, status, Unread, Pinned, Archived, time); freshness text starts with its state. E2E P03.2; native QA with VoiceOver. |
| F15 | Edge, demo or pairing regressions | The shell only replaces sign-in when signed out with a Mac; edge screens stay dark; `remote.open` stays the Paired Macs entry. Existing `ProjectSessionUITests`, `MobilePolishTests`, `AppshotUITests`, `RemotePairingUITests`, `RemoteTasksRuntimeTests`. |
| F16 | Drafts or routes leak or grow without bound; a secret in preferences | Bounds above; drafts protected and excluded from backup; the route record holds ids and a search string only. Coordinator `secretKeysInPreferences` is empty. |
| F17 | Forget leaves the Mac's cache or drafts | Forget removes both. E2E P03.14 plus `lunaRemoteFiles` in the summary (expected empty on a disposable Simulator). |
| F18 | More followed tasks than the core allows, or a rebuilt runtime stops following | `follow(_:)` keeps four and re-follows after `prepareLive`. E2E P03.6/P03.11 (live after relaunch and restart). |
| F19 | Two windows fight over one runtime | Mac selection is app-wide by design; multiple windows are not enabled (`UIApplicationSupportsMultipleScenes` absent). Limitation. |

## Verification rounds

**Round 2 (orchestrator, 14 October):** checks, build, `LunaTests` (117, 0
failures), demo regressions (14/14) and the unpaired continuity compare
passed. Three findings, fixed in code:
1. iPhone E2E, P03.7: Command-] right after a relaunch did not change the
   task. The SwiftUI shortcuts are reached only through the first
   responder, and a relaunched app had none (Command-F had worked after
   taps). Fix: `ShellKeyCommands`.
2. iPad E2E, P03.4: tapping a row in the regular-width list neither selected
   it nor opened the task (no highlight; the detail stayed "Select a task").
   Fix: rows (and the sidebar's project rows) are `NavigationLink(value:)`
   inside `List(selection:)`, the split-view pattern that drives the next
   column in both widths; the row's label and identifier are on the link.
3. Appearance migration: after an unpaired baseline run, the upgrade wrote
   no `luna.appearance.v1`. The migration lived in the lazily created state
   object. Fix: `LunaApp.init` runs `AppearanceMigration.migrate()` first
   and flushes the one write.

**Round 3 (orchestrator, 14 October):** build and `LunaTests` (117, 0
failures) passed; the appearance migration reads back `dark` after an
unpaired baseline and `system` on a fresh install (round 2's "missing" was
the reader: `plutil -extract` cannot address a dotted key). One regression:
on iPhone, Command-F no longer focused search (P03.3), because the new
responder received it and SwiftUI's focus request did not take the keyboard
from it. Fix: Command-F makes the responder step aside, asks SwiftUI on the
next turn, then makes the visible search field first responder if no text
input is; the responder never reclaims while a text input edits.

**Round 4 (orchestrator, 14 October):** the same iPhone failure at P03.3:
the bottom search field still did not get the keyboard after Command-F.
Round 1, without the UIKit responder, had passed that step. Fix, narrowed
to the case the responder was added for: it carries only Command-] and
Command-[, claims only when it appears or the app becomes active (no claims
after the keyboard hides or a field ends editing), never while a text input
edits, and resigns when editing begins or when the SwiftUI Command-F posts
`shellYieldKeyboard` just before setting the search focus. Command-F,
Command-R and Command-, are the round-1 SwiftUI shortcuts again.

**Round 5 (orchestrator, 14 October):** Command-F failed on both devices at
P03.3 with the narrowed responder too, while round 1 (no responder) passed
there on both. Conclusion: SwiftUI shortcuts work with no first responder
(round 1's Command-F), and the UIKit responder holding first-responder
status is what broke Command-F. Round 1's Command-] failure is explained by
where the shortcut lived instead: in the split view's root background while
the task column was on top of the iPhone stack. Fix: `ShellKeyCommands` is
removed. The root keeps the round-1 Command-F, Command-, and Command-R;
Command-F now also presents search (`searchable(text:isPresented:)`).
Command-[ and Command-] are on the task column's visible Previous/Next
toolbar buttons (also on its empty state), Command-R on its Retry buttons,
and Command-, on a Settings button that toolbar shows in compact width.

**Round 6 (orchestrator, 14 October):** Command-F still failed at P03.3 on
iPhone with the responder gone, so it was not the cause. To isolate it, the
search path is round 1's again: tagged task rows, `.searchable(text:prompt:)`
with `.searchFocused`, Command-F setting only the focus. The iPad tap is
fixed without `NavigationLink`: each task row is a full-row `Button` (plain
style, `contentShape(Rectangle())`) that sets the selection and the compact
column, as restoring a route does, with the row's label and
`shell.task.<id>` on the button and its tag kept for the highlight. The
sidebar's project rows are round 1's tagged rows. Command-[ and Command-]
stay on the task column's Previous/Next buttons; the empty "Select a task"
state no longer has that toolbar.

**Round 7 (orchestrator, 14 October):** the Command-F failures of rounds
2-6 were environmental: on erased Simulators the round-6 code passes
Command-F on both devices. So the conclusions of rounds 2-5 about Command-F
(and about the UIKit responder breaking it) rest on a stale Simulator. The
runs reach round 1's failure points again: Command-] right after a
relaunch on iPhone, and a row tap that opens nothing on iPad. Fixes:
- `ShellKeyCommands` is back exactly as round 4 narrowed it (only
  Command-] and Command-[; claims on appearance or activation with no
  responder and no text input editing; yields on editing and on
  Command-F's `shellYieldKeyboard`).
- The task list keeps its own selection state. It follows the route for
  the highlight; a selection the list makes opens that task, as does the
  row's full-row button, through one `open` (route, then compact column);
  in compact width a cleared selection (back from the task) leaves it.
- The task column's view is keyed by the task id, so another task always
  gets a fresh view instead of an in-place update (both failures changed
  the task while the column already existed).

## Criteria mapping

- **contract:** this document: ownership, route recovery, connection states,
  organization, supported actions and the appearance migration;
  `ShellModel.swift`, `ShellRestoration.swift`, `ShellDrafts.swift`,
  `AppearancePreferences.swift`.
- **delivery:** `MacShellView.swift` (split/stack routing, sidebar, list,
  filters, search, project menu, commands), `ShellTaskDetailView.swift`,
  `ShellSettingsView.swift`, the adaptive `Theme`, and the root wiring, all
  over RM-03's projections through `PairedMacsModel`.
- **verification:** P03 native E2E on iPhone and iPad, the tables, the
  regression suites, the continuity QA and the native QA steps below.
- **handoff:** the orchestrator's builds, runs, evidence and delivery. The
  Mac app is unchanged (only an engine test and an iOS QA script changed on
  the Mac side), so no LUNA Dev refresh is needed for this change.

## Capabilities this needs from RM-04 (not implemented here)

The shell stops at the Swift boundary; RM-04 owns the facade and the
regenerated bindings. Its in-progress core (`crates/mobile/src/session/
commands.rs`, facade 3) already names what the hooks below would call:
- **Archive and restore:** `RemoteSession.setTaskArchived(taskId:archived:)`
  (returns the command id) with `outbox()` / `outboxRevision()` /
  `outboxIssue()` for its pending, refused or applied state. Hook: Archive and
  Restore in the row's context menu and swipe actions, shown only when the
  projection's capabilities include `remote-task-control-v1`, the saved
  access includes `tasks-operate` and `outboxIssue()` is nil; the row stays
  marked pending until the outbox entry is `applied` or `refused`, and the
  Mac's next projection is what moves it between All and Archived.
- **Pin and mark read:** not in RM-04's closed `RemoteTaskIntent` set. The
  phone needs remote `setPinned { pinned, afterTaskId?, beforeTaskId? }` and
  `setRead { read }` intents (and facade calls) before it can offer them.
- **Draft to created task:** `createTask(projectId:)` returns the phone-minted
  task id, and messages to it stay outbox drafts until the Mac creates it.
  IOS-03's composer moves a new-task `DraftBook` entry to that id (it adds
  `DraftBook.adopt(account:from:to:)` together with its caller) and sends with
  `sendMessage`.

When RM-04 lands, the list's note changes by itself to "This version of LUNA
for iPhone shows tasks only." (the host then advertises task control) until
these hooks are wired.

## Commands (orchestrator)

Use an owned, disposable iPhone Simulator (`$SIM`) and iPad Simulator
(`$IPAD`, for example `xcrun simctl create "IOS02 iPad"
com.apple.CoreSimulator.SimDeviceType.iPad-Air-11-inch-M4
com.apple.CoreSimulator.SimRuntime.iOS-26-4`). DerivedData stays outside
`target/`: `DD=<scratch>/ios02-dd`. `<run>` is
`target/qa/ios-02/<UTC stamp>`.

**1. Rust (the engine test file changed):**

```sh
scripts/cargo.sh check --locked -p luna-engine --tests
scripts/cargo.sh clippy --locked -p luna-engine --test remote_access
```

**2. iOS build and unit tests (from `apps/ios`):**

```sh
xcodebuild -project Luna.xcodeproj -scheme Luna -configuration Debug \
  -destination "platform=iOS Simulator,id=$SIM" -derivedDataPath "$DD" build-for-testing
git diff --exit-code -- Luna/Generated/luna_core.swift     # unchanged by this ticket
xcodebuild test-without-building -project Luna.xcodeproj -scheme Luna \
  -destination "platform=iOS Simulator,id=$SIM" -derivedDataPath "$DD" \
  -only-testing:LunaTests -resultBundlePath "<run>/LunaTests.xcresult"
```

Expected: `ShellPreferencesTests` 2 passed; the fixture-driven remote tests
skip; everything else as before.

**3. Native E2E, once per device (repository root):**

```sh
xcrun simctl install $SIM "$DD/Build/Products/Debug-iphonesimulator/Luna.app"
xcrun simctl install $SIM "$DD/Build/Products/Debug-iphonesimulator/LunaUITests-Runner.app"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test remote_access --no-run
python3 scripts/ios/qa-native-session-sim.py --campaign ios02 --simulator $SIM \
  --run-dir <run>/iphone --host-ip <this Mac's Wi-Fi IP> &
# after it prints "Native fixture ready":
xcodebuild test-without-building -project apps/ios/Luna.xcodeproj -scheme Luna \
  -destination "platform=iOS Simulator,id=$SIM" -derivedDataPath "$DD" \
  -parallel-testing-enabled NO -only-testing:LunaUITests/AdaptiveShellUITests \
  -resultBundlePath <run>/iphone/AdaptiveShellUITests.xcresult
```

Repeat with `$IPAD` and `<run>/ipad`. Expected for each: 1 test passed, 0
skipped; coordinator exit 0; `summary.json` with every `nativeObservations`
flag true (`splitShowsListAndTask` too on iPad), `deviceId` equal to the
engine's, empty `secretKeysInPreferences` and empty `lunaRemoteFiles`.
Export the screenshots (`iphone-*`, `ipad-*`: live list, portrait and
landscape detail, removed, stale, cached offline, light, dark, largest text)
from the result bundle with `xcrun xcresulttool export attachments --path
<bundle> --output-path <run>/<device>/attachments`, hash them, and publish
only `summary.json`, the screenshots and the hashes. The test runs about 4–6
minutes and ends with Appearance set to System.

**4. Regression suites (demo-based; `$SIM`):**

```sh
xcodebuild test-without-building -project apps/ios/Luna.xcodeproj -scheme Luna \
  -destination "platform=iOS Simulator,id=$SIM" -derivedDataPath "$DD" \
  -only-testing:LunaUITests/ProjectSessionUITests -only-testing:LunaUITests/MobilePolishTests \
  -only-testing:LunaUITests/AppshotUITests -resultBundlePath <run>/regressions.xcresult
```

Then RM-02's `RemotePairingUITests` through `scripts/qa-native-pairing-sim.py`
and RM-03's `RemoteTasksRuntimeTests` through `--campaign rm03`, as their
contracts describe (the shell now replaces sign-in after the first pairing;
`remote.open` is the shell's Paired Macs button).

**5. Appearance and data continuity** (`$SIM`, a QA LUNA app as the host):
1. Install a baseline build from `729cc458` (a detached QA worktree with its
   own DerivedData), pair the QA host, quit the QA host gracefully, terminate
   the app, then
   `python3 scripts/ios/qa-data-continuity-sim.py snapshot --simulator $SIM --out <run>/baseline.json`.
2. Install this build over it; launch (the Mac is down, so nothing connects
   or saves); terminate; `snapshot --out <run>/upgraded.json`;
   `compare <run>/baseline.json <run>/upgraded.json --allow 'defaults:luna.appearance.v1' --allow 'defaults:luna.shell.v1'`.
   Expected: only those two added;
   `plutil -extract luna.appearance.v1 raw "$(xcrun simctl get_app_container $SIM com.novnski.luna.ios data)/Library/Preferences/com.novnski.luna.ios.plist"`
   prints `dark`, and Settings shows Dark.
3. Downgrade: install the baseline over it and launch: dark, the paired Mac
   listed; `compare` against `upgraded.json` shows no removal.
4. Fresh install: `xcrun simctl uninstall $SIM com.novnski.luna.ios`, install
   this build, pair, open Settings: System.

## Native QA (the orchestrator, booted iPhone 17 Pro and an iPad, paired to a QA LUNA app)

Use a QA LUNA app with all four `LUNA_DEV_*` overrides and its own data
([QA guide](../../../reference/qa-infrastructure.md)); never the primary
profile. Pair from the QA app's Settings → Connections → Control this Mac →
Add (manual code). Mask the code in any capture.
1. **Shell:** the app opens on the Mac's tasks; the identity line names the
   QA Mac's host id; the freshness reads `Live · confirmed …`.
2. **Organization:** each filter chip and its count; the project menu (and
   the iPad sidebar); search; Clear filters; a pinned task in Pinned.
3. **iPad split and multitasking:** open a task, type a draft; put LUNA in
   Split View with another app and drag the divider to one third (compact)
   and back; also Slide Over and a Stage Manager window resize. The same task
   and draft stay; collapsing and expanding never opens another task.
4. **Rotation:** both devices, list and detail, portrait and landscape.
5. **Hardware keyboard (iPad):** Command-F, Command-] and Command-[,
   Command-R, Command-,; up and down arrows move the list selection; holding
   Command shows the shortcuts.
6. **VoiceOver** (Settings → Accessibility → VoiceOver, or Accessibility
   Inspector): a row reads title, project, status, Unread/Pinned, time; the
   list freshness and the detail freshness read their state first; the state
   screens read their reason and Retry.
7. **Large text and Reduce Motion:** the largest Larger Text size keeps rows,
   filters, Retry, the draft and the toolbar reachable; Settings reads the
   text size and Reduce Motion back from iOS.
8. **Light, Dark, System:** each choice, then the iOS appearance toggled with
   System selected; review contrast in both.
9. **Deleted and offline:** delete the open task in the QA app (removed
   state, draft kept); quit the QA app gracefully (`Stale … Reconnecting…`);
   relaunch the phone app (`Cached on this phone …`, Can't reach, Retry);
   relaunch the QA app and Command-R or Retry (`Live`).
10. Afterwards revoke the QA grant on the QA app, Forget it on the phone,
    quit the QA app gracefully, and erase only disposable Simulators.

## Limitations

- **Read-only list actions.** Archive, pin and mark read wait for RM-04 (see
  above); the phone sends nothing to the Mac in this ticket.
- **Drafts are not sent.** They are local and say so; IOS-03 owns the
  composer and the new-task draft handoff.
- **One window.** Per-scene state is keyed and restorable, but multiple
  windows are not enabled, and the selected Mac is app-wide (one runtime).
- **Pairing another Mac pauses the selected one.** RM-02's `prepare` closes
  the current runtime when a code is reviewed; after cancelling, the shell
  shows "Not connected" with Connect. Unchanged from RM-02/RM-03.
- **Cold start reconnects.** The restored Mac is connected when the shell
  first appears (RM-02 previously waited for the sheet's Connect). Hosted
  `LunaTests` run inside the app, so its shell may connect to a Mac saved on
  that Simulator; use disposable Simulators as the RM campaigns already do.
- **Split/multitasking is native QA, not XCUITest.** XCUITest cannot drive
  Split View or Stage Manager; rotation and the iPad two-column layout are in
  the E2E.
- **Simulator only.** Physical-device VoiceOver, keyboard, performance and
  real network transitions are IOS-09 and RM-07.
- **Edge screens stay dark.** Their transcript and composer force dark and
  are IOS-01/IOS-03's; the launch screen stays dark too.
- **History is RM-03's bounded tail** (150 entries); the native transcript is
  IOS-01's.
- **A relaunch right after a foreground termination has no cache** until the
  app has been backgrounded once (RM-03 saves projections on background); the
  restored route then shows its loading or unavailable state, never stale
  content as current.
