# CORE-19: plugin updates, rollback, removal and safe mode

Baseline: `6a0ad25c` (13 October 2026). Live status belongs to
`pending_tasks.py`. This file is the contract, the failure cases (written
before the code), the criteria mapping and the evidence plan.

## Rebased against current main

What already existed and is reused:
- **CORE-06** (`core-06/README.md`): immutable, content-addressed packages
  (`{data dir}/extensions/packages/<hash>/`, `installed.json`), the
  stage → verify → commit installer, per-profile grants
  (`{store root}/extension-grants.json`, temp file + fsync + rename) and the
  reserved data scope `{store root}/extensions/data/<id>/`. Installing a second
  version of an id already gave a staged, verified, disabled candidate; the
  only way to switch was disable, then enable with a new grant set.
- **CORE-07** (`core-07/README.md`): the supervisor and broker, the
  per-profile runtime state (`extension-runtime.json`) with durable
  quarantine, principals bound to one exact package hash, calls that fail
  `workerExited` and are never replayed. Its hand-off: updates must drain or
  stop instances pinned to the old hash; removal must release that hash's
  quarantine record.

What was missing: any version switch, drain, record of unknown outcomes,
data generations or migration, removal, data erasure, or safe mode. No
existing test owned these, so one new engine E2E owner and one UI owner were
added (test-audit gate: behavior, credible regression, strongest boundary).

## Contract

Types: `crates/proto/src/extension_lifecycle.rs`, plus additions to
`extension_packages.rs` and `extension_runtime.rs`. Extension API is now
**1.2** (adds `data.migratesFrom`, the `data.read`/`data.write` broker
operations and the `luna.migrate`/`luna.reconcile` worker operations).

### Owner methods

All are catalog rows with `local` target and `localOnly` authority, served
only by the engine's local service (in-process app and owner IPC). Workers
and agents never reach them. Unknown fields are refused, so a forged
`confirmed: true` is `bad params`. Each runs to completion in its own task
even if the caller disconnects.

| Method | Request | Reply |
| --- | --- | --- |
| `UpdateExtension` | `{extensionId, fromPackageHash, toPackageHash, grants, active: drain\|stop, drainTimeoutMs?, migrationTimeoutMs?}` (`grants` has no default) | `ExtensionUpdateOutcome` |
| `RemoveExtension` | `{extensionId, packageHash}` | `ExtensionRemoveOutcome` |
| `EraseExtensionData` | `{extensionId}` | `ExtensionEraseOutcome` |
| `SetExtensionSafeMode` | `{enabled}` | `ExtensionsSnapshot` |

`CallExtensionInstance` gains `requestId?`; its outcome gains `callId` and
`requestId`. `ListExtensions` gains `safeMode`, `data`, and per package
`quarantineReason`, `dataMigratesFrom` and `switch`. `ListExtensionInstances`
gains `unresolved` and `safeMode`; each instance gains `purpose` (`run` or
`migration`) and `dataSchemaVersion`. New instance state `draining`; new exit
reasons `updating` and `safeMode` (neither is a fault).

### Version switch (update or rollback)

1. **Stage and verify** is the CORE-06 install of the candidate: a disabled
   package of the same id. Nothing changes for the enabled version.
2. **Explicit activation** is `UpdateExtension`, naming both exact hashes.
   Refused (and nothing stops or changes) when: `from` is not this profile's
   enabled package; `to` is not installed under that id; `to` fails a full
   re-hash; `to` is quarantined; a dependency of `to` is not enabled in range;
   an enabled dependent's range excludes `to`'s version; the grant set is not
   exact (unrequested, duplicate, or a required one missing); the data cannot
   be carried (below); a migration would run in safe mode. API, platform and
   runtime compatibility are the CORE-06 manifest checks, re-run whenever the
   installed manifest is read.
3. **Grants never carry over.** The stored grant for `to` is exactly the
   request's set. `switch.newCapabilities` lists what `to` requests that the
   enabled version was not granted; `expandedGrants` in the outcome lists the
   ones the request granted. Grants `to` no longer requests are dropped.
4. **Pinned instances.** Every instance stays bound to the hash it started
   with. `drain` (default, 30 s, at most 120 s) marks the extension's running
   instances `draining`: new calls are refused, starts and restarts of the id
   are refused, calls in progress may finish. If any is still running at the
   deadline, the instances return to `running` and nothing changes. `stop`
   stops them now. Either way they end `stopped` with exit reason `updating`.
5. **Data** (see below) is migrated on a copy or restored, then
6. **one commit**: a single grants-file write (temp, fsync, rename) names the
   new package, its grants and its current data generation. A crash before
   the rename leaves the old version with its data; after it, the new one with
   its data. No other file decides which version is active.

### Unknown outcomes are reconciled, never replayed

A call still waiting for its answer when its worker exits (crash, stop,
update, safe mode, shutdown) may or may not have taken effect. The supervisor
records it in the profile's runtime state (`unresolved`: call id, request id,
extension, version, hash, instance, generation, method, times, exit reason),
bounded at 256 records with settled ones evicted first. Host `luna.*`
operations and migration instances are settled by their own caller and are
not recorded.

- A `CallExtensionInstance` whose `requestId` matches an unsettled record of
  the same extension is refused; one settled `applied` is refused too; one
  settled `notApplied` may be sent again. A new request id is a deliberate new
  command.
- Each time a worker of the extension (any version) starts or restarts and
  advertises `luna.reconcile`, the supervisor offers it up to 64 unsettled
  records (5 s deadline) and records each `applied`/`notApplied` answer with
  the answering version and hash. `unknown` or missing answers stay unsettled.
  Nothing is re-sent by LUNA.

### Plugin data and migrations

- Layout: `{store root}/extensions/data/<id with / as +>/<generation uuid>/`.
  The grants file's `data` map holds each id's `current` generation and the
  `kept` older ones (newest first, at most 2), each with its schema, the
  version and hash that wrote it, and when. Data outlives enablement and
  installation.
- A file with data pointers is written as grants version 2 (and a runtime
  state file with safe mode or unresolved calls as version 2), which an older
  LUNA refuses and preserves instead of rewriting without them.
- Enabling a package with `data` creates an empty generation if the profile
  has none, and is refused if the profile's data has another schema (enabling
  never migrates). Instances get the current generation through the `data.*`
  broker operations; restarts are refused if it changed.
- A switch picks, from the profile's current schema `c` and `to`'s schema `t`:
  no data in `to` → untouched; no data yet → fresh; `c == t` → keep;
  `c < t` → migrate if `to.data.migratesFrom <= c`, else restore an exact
  kept copy of `t`, else refuse; `c > t` → restore an exact kept copy of `t`,
  else refuse (older code never reads a newer schema).
- Migrate: copy the current generation (regular files and folders only;
  links refused; 1 GiB, 100,000 entries), start a `migration` instance of `to`
  bound to the copy, require `luna.migrate`, call it with the deadline. Any
  failure (refused handshake, error, crash, deadline) deletes the copy; the old
  generation is never written. Success promotes the copy at the commit and
  keeps the old one. Restore promotes the kept copy and keeps the current one.

### Removal and erasure

- `RemoveExtension` refuses while an enabled package depends on it. It stops
  and revokes the hash's instances, then: deletes this profile's grant record
  if it names the hash, unlists the package (the commit point), releases its
  quarantine record and fault count, deletes its tree. Recovery on open
  finishes an interrupted removal: an unlisted tree is removed (CORE-06) and a
  quarantine record of a hash that is not installed is dropped.
- Data is kept (`dataKept`) and listed (`installed: false`) until
  `EraseExtensionData`, which is refused while an instance of the id runs. It
  renames the id's folder aside, deletes it, then deletes the record; an
  interrupted erase is finished on open.

### Safe mode

- On: the profile's runtime state holds `safeMode`; or the engine was launched
  with `LUNA_EXTENSIONS_SAFE_MODE=1` (this run only). Turning it on stops
  every worker (`safeMode`). While on, every start and restart is refused
  (`check_startable`, which every activation path calls), and switches that
  would run a migration are refused.
- Grants, packages and data are untouched. Install, review/enable/disable,
  switches that keep or restore data, removal, quarantine release and erasure
  keep working, from Settings → Extensions or owner IPC. LUNA's built-in
  providers are not extension packages and keep working offline; there is no
  marketplace dependency.

### UI (Settings → Extensions)

Rows show `Enabled`/`Off` and `Quarantined`; actions are `Disable`, `Review
and enable`, `Update to vX` / `Roll back to vX` (disabled with the reason when
the switch is blocked), `Release`, `Remove…`. The switch review shows both
versions, the data plan, dropped grants, every capability the enabled version
was not granted marked `not granted to vX`, required ones locked on, those
optional ones off, and, with running
instances, a "Stop running work instead of waiting" switch. `Remove…` and
`Erase data…` confirm first. A `Safe mode` title action turns it on; the
banner explains it and offers `Turn off safe mode` (or says it came from the
launch).

## Failure contract (written before the code)

| ID | Failure | Defence |
| --- | --- | --- |
| U1 | A tampered candidate is staged or installed | CORE-06 staged verify; nothing listed |
| U2 | A switch with an unmet dependency or a dependent's range broken proceeds | `to`'s dependencies against enabled versions; every enabled dependent's range |
| U3 | A switch silently widens grants (carries old grants, grants an added capability) | The stored set is exactly the request's; `grants` is required; new capabilities listed and off in the UI |
| U4 | A refused switch stops the running worker or rewrites grants | All checks run before quiesce; one commit write |
| U5 | A stale `from` or a concurrent change switches the wrong version | `from` must be the enabled hash, re-checked at the commit (compare-and-swap) |
| U6 | A package changed after install is activated | Full re-hash in the switch |
| U7 | A worker is replaced under a call in progress | Drain waits; the old instance stops before the commit; the new version starts separately |
| U8 | A new call or start reaches the old version during the switch | `draining` refuses calls; the id is claimed, so starts and restarts are refused |
| U9 | A drain that cannot finish switches anyway | Timeout restores `running` and returns an error; nothing changed |
| U10 | An interrupted call is replayed, or a resend of an unknown/applied request runs again | Never re-sent; the ledger refuses the request id until settled `notApplied` |
| U11 | Reconciliation is lost on restart or trusts a forged call id | Durable ledger; only offered call ids are accepted |
| U12 | A failed, crashed or impossible migration leaves half-migrated data current or touches the old data | Migration on a copy; copy deleted on failure; old generation never written |
| U13 | A migration worker outlives its failure | It is stopped (or never started); `migration` purpose, never restarted |
| U14 | A rollback reads a newer, irreversible schema | Restore an exact kept copy or refuse; enable refuses another schema |
| U15 | Power loss mid-switch leaves a mix of versions or data | One grants-file rename commits package, grants and data together; recovery removes uncommitted copies |
| U16 | Removal under an enabled dependent | Dependents check |
| U17 | A quarantine outlives its package (blocks a clean reinstall) | Released on removal; pruned on open for hashes not installed |
| U18 | Uninstall erases data, or erase leaves a copy or pointer | Separate commands; erase deletes every generation and the record |
| U19 | Erasing data in use | Refused while an instance of the id is live |
| U20 | An extension runs in safe mode, or safe mode is lost on restart | `check_startable` refuses; profile setting is durable; launch override |
| U21 | Safe mode changes enablement or data, or blocks recovery | Nothing is rewritten; management, keep/restore switches, removal and erase work |
| U22 | An older LUNA rewrites the new state without its data pointers or ledger | Version 2 files, refused and preserved by older readers |

| Owner test (`crates/engine/tests/extension_lifecycle.rs`) | Cases |
| --- | --- |
| `refused_installs_and_switches_change_nothing_and_grants_never_widen` | U1–U6 (E08) |
| `active_runs_drain_or_stop_and_unknown_outcomes_are_reconciled_not_replayed` | U7–U11, U22 (E09/E16) |
| `removal_releases_quarantine_and_keeps_data_until_it_is_erased` | U16–U19 (E09/E16) |
| `migrations_commit_on_a_copy_and_rollback_never_reads_a_newer_schema` | U12–U15, U17, U22 (E16) |
| `safe_mode_starts_with_extensions_off_and_still_recovers` | U20, U21 (E16) |
| UI: `settings::extensions::lifecycle_tests::lifecycle_states_and_actions_follow_the_engine` | Visible switch/rollback/remove/erase/safe-mode states and their exact engine effects (U3 at the review) |

Independent readback in every engine test: OS process existence
(`kill(pid, 0)`), `installed.json`, package trees, the grants file and
runtime-state file as JSON, SHA-256 digests of every data generation folder
at its published path, and the worker's own journal and side-effect files.
The worker is an independently written Python peer; package hashes are
recomputed from the published rule. Power loss is reproduced as the exact
residue each durable step leaves (a complete uncommitted copy plus a complete
would-be commit in a grants temp file, an erase renamed aside, a quarantine of
a removed hash), followed by a reopen, as the CORE-06 suite does; a migration
worker that really fails or dies mid-migration covers the live interruption.

These risks are **not tested**:
- `LUNA_EXTENSIONS_SAFE_MODE` (process-global environment): native QA only.
- Kept-copy eviction beyond two generations and the 256-record ledger bound.
- An engine killed (not shut down) during a drain or migration in progress:
  the residue cases cover its on-disk outcome, not the process kill itself.
- Portable (Wasm) migrations: the path is the same supervisor call, but no
  Wasm guest implements `luna.migrate` in a test.
- Non-macOS platforms: the suite is `#![cfg(unix)]` and written on macOS.

## Criteria mapping

- **contract:** this file and `crates/proto/src/extension_lifecycle.rs`
  (version pins, drain/stop, unknown-outcome reconciliation, dependency/API
  compatibility, explicit grants, data migration and rollback rules, removal,
  erasure, safe mode).
- **delivery:** `crates/engine/src/extensions.rs` (switch, removal, erase,
  safe mode, data binding), `extensions/data.rs` (generations, copy, erase,
  recovery), `extensions/grants.rs` (data pointers, version 2),
  `extensions/store.rs` (removal), `extensions/manifest.rs`
  (`data.migratesFrom`), `extensions/runtime.rs` (pinning, drain, retire,
  safe mode, reconciliation, request ids), `runtime/{worker,broker,quarantine}.rs`
  (interrupted calls, `data.*`, ledger and safe-mode state), the RPC handlers
  in `crates/engine/src/rpc/extensions.rs`, and Settings → Extensions in
  `crates/ui/src/settings/extensions.rs`.
- **verification:** E08/E09/E16 are the five engine tests and the UI test
  above, with the readback listed.
- **handoff:** the orchestrator's build, E2E, lint, review, LUNA Dev refresh
  and delivery.

## Commands (orchestrator)

```sh
python3 scripts/build-budget.py --status
scripts/cargo.sh check --locked -p luna-proto -p luna-rpc -p luna-engine
scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract   # operations.json (+4 rows; already added by hand)
scripts/cargo.sh test --locked -p luna-rpc --features schema --test contract
scripts/cargo.sh test --locked -p luna-rpc --lib remote_auth::tests::policy_inventory_is_complete_and_unknown_fails_closed
EVIDENCE="$PWD/target/qa/core19/$(date -u +%Y%m%dT%H%M%SZ)"
LUNA_CORE19_EVIDENCE_DIR="$EVIDENCE" CARGO_INCREMENTAL=0 \
  scripts/cargo.sh test --locked -p luna-engine --test extension_lifecycle -- --nocapture
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine \
  --test extension_packages --test extension_runtime
scripts/cargo.sh build --locked -p luna-extension-host
LUNA_EXTENSION_HOST="$PWD/target/debug/luna-extension-host" CARGO_INCREMENTAL=0 \
  scripts/cargo.sh test --locked -p luna-engine --test extension_portable
scripts/cargo.sh check --locked -p luna-ui
LUNA_CORE19_EVIDENCE_DIR="$EVIDENCE" scripts/cargo.sh test --locked -p luna-ui --lib \
  lifecycle_states_and_actions_follow_the_engine -- --test-threads=1
scripts/cargo.sh test --locked -p luna-ui --lib settings_entry_and_fixture_routes -- --test-threads=1
scripts/cargo.sh clippy --locked -p luna-proto -p luna-rpc -p luna-engine -p luna-ui --all-targets
(cd "$EVIDENCE" && shasum -a 256 *.json > SHA256SUMS)
```

The engine suite needs `python3` on `PATH`, like the CORE-07 native suite.
The UI test uses data-only packages and runs in the existing `luna-ui --lib`
CI job; the engine suite is not added to CI (the CORE-07 native suites are
not in CI either).

## Native QA (LUNA Dev, local data only)

Make the QA packages from the test's worker (the script extracts
`LIFECYCLE_WORKER` from the test source, so QA runs the same peer):

```sh
agent-python -I - "$PWD/crates/engine/tests/extension_lifecycle.rs" <<'PY'
import hashlib, json, os, sys
source = open(sys.argv[1]).read()
worker = source.split('const LIFECYCLE_WORKER: &str = r##"', 1)[1].split('"##;', 1)[0].encode()
root = "/tmp/core19-qa"
plat = {"darwin": "macos"}.get(sys.platform, sys.platform) + "-" + os.uname().machine.replace("arm64", "aarch64")
def pkg(version, data, fixture, capabilities=()):
    d = f"{root}/notes-{version}"; os.makedirs(d + "/bin", exist_ok=True)
    fixture = dict(fixture, version=version, journal=f"{root}/journal.jsonl", effects=f"{root}/effects.txt")
    files = {"bin/worker": worker, "fixture.json": json.dumps(fixture).encode()}
    for p, b in files.items(): open(f"{d}/{p}", "wb").write(b)
    m = {"manifestVersion": 1, "id": "core19.qa/notes", "version": version, "displayName": "QA notes",
         "publisher": "CORE-19 QA", "api": {"major": 1, "minMinor": 2},
         "runtime": {"kind": "native", "protocol": {"major": 1, "minMinor": 0}, "entries": {plat: "bin/worker"}},
         "capabilities": list(capabilities), "data": data,
         "files": [{"path": p, "size": len(b), "sha256": hashlib.sha256(b).hexdigest()} for p, b in files.items()]}
    raw = json.dumps(m, indent=2).encode(); open(f"{d}/luna-plugin.json", "wb").write(raw)
    print(d, hashlib.sha256(raw).hexdigest())
pkg("1.0.0", {"schemaVersion": 1}, {"reconcile": True})
pkg("2.0.1", {"schemaVersion": 2, "migratesFrom": 1}, {"migrate": "fail"})
pkg("2.0.0", {"schemaVersion": 2, "migratesFrom": 1}, {"migrate": "ok", "reconcile": True},
    [{"id": "luna.capability/tasks.read"}, {"id": "luna.capability/network", "required": False}])
PY
```

Owner IPC helper (Node's built-in WebSocket, port 59777), used below as
`ipc METHOD 'JSON'`:

```sh
ipc() { node -e '
const [m, p] = process.argv.slice(1); const ws = new WebSocket("ws://127.0.0.1:59777");
ws.onopen = () => ws.send(JSON.stringify({id: 1, method: m, params: JSON.parse(p || "{}")}) + "\n");
ws.onmessage = ({data}) => { console.log(JSON.stringify(JSON.parse(String(data).split("\n")[0]), null, 2)); ws.close(); };' "$1" "$2"; }
```

In LUNA Dev:
1. Settings → Extensions → Install… `/tmp/core19-qa/notes-1.0.0` → Review and
   enable → Enable. Then `ipc StartExtensionInstance '{"extensionId":"core19.qa/notes","packageHash":"<1.0.0 hash>"}'`
   and `ipc CallExtensionInstance '{"instanceId":"<id>","method":"fixture.put","params":{"path":"state.json","text":"{\"schema\":1,\"count\":5}"}}'`.
   Record `shasum -a 256` of `<store root>/extension-grants.json` and of
   `<store root>/extensions/data/core19.qa+notes/*/state.json`.
2. Install `notes-2.0.1`. Its row shows **Update to v2.0.1**; the review says
   the data is migrated on a copy. Update: the error strip says the migration
   failed and 1.0.0 is still enabled. Both hashes from step 1 are unchanged;
   the data folder still holds one generation; `ps` finds no worker of 2.0.1.
3. Install `notes-2.0.0` → **Update to v2.0.0**. The review marks
   `tasks.read` and `network` "not granted to v1.0.0"; `tasks.read` is
   locked on (required) and `network` is off. Update. The grants
   file names 2.0.0 with exactly `native-code` and `tasks.read`; the data
   record is schema 2 with schema 1 kept; the schema-1 file is byte-identical.
4. The 1.0.0 row shows **Roll back to v1.0.0** (restores the schema-1 copy).
   Roll back; start 1.0.0 over IPC and `fixture.get` `state.json`: the
   original text.
5. Active run: start 1.0.0, send `fixture.apply` with
   `"params":{"line":"x","hold":"after"},"requestId":"qa-x"` (it never
   answers), then in the UI update to 2.0.0 with **Stop running work instead
   of waiting** on. `ListExtensionInstances.unresolved` shows `qa-x`; start
   2.0.0 and list again: settled `applied` by 2.0.0. Resending `qa-x` is
   refused; `/tmp/core19-qa/effects.txt` has `x` once.
6. Safe mode: choose **Safe mode**. The banner appears; the running instance
   stops; `ipc StartExtensionInstance …` is refused. Quit gracefully,
   relaunch: the banner is still there; start a normal chat with a built-in
   provider to confirm the core works. **Turn off safe mode**.
7. Launch-time safe mode: quit gracefully, then
   `open --env LUNA_EXTENSIONS_SAFE_MODE=1 'target/luna-macos-dev/LUNA Dev.app'`
   (or run its `Contents/MacOS` binary with the variable). The banner says it
   is for this launch and has no turn-off button. Quit and relaunch normally:
   it is gone.
8. Crash loop and removal: `fixture.crash` three times on a 2.0.0 instance
   (each call restarts it): the row shows **Quarantined** with **Release**.
   Remove… 2.0.0 (confirm): the row is gone, the quarantine is released
   (`ListExtensionInstances.quarantined` empty), `<data dir>/extensions/packages/<hash>`
   is gone, and **Extension data in this profile** still lists the data.
   Remove 2.0.1 and 1.0.0, then **Erase data…** (confirm): the folder
   `<store root>/extensions/data/core19.qa+notes` is gone.
9. Capture screenshots and AX labels of the switch review, both
   confirmations, the safe-mode banner (profile and launch) and the
   quarantined row.

## Limitations and hand-offs

- **Engine kill:** CORE-07's limitation stands: a worker that ignores end of
  input outlives a killed engine. The on-disk switch is still all-or-nothing.
- **Other profiles on the device:** removal deletes a device-scoped package
  while another profile may have it enabled; that profile's grant then names
  a missing hash and fails closed (nothing starts, the row is gone). Its data
  stays in that profile.
- **Native workers' data:** brokered `data.*` operations are confined, but a
  native worker can reach its data folder (and the rest of the account)
  directly; it is not sandboxed (CORE-07).
- **Reconciliation** depends on the extension: without `luna.reconcile`,
  unknown outcomes stay unsettled and only a new request id sends the work
  again. Concurrent duplicate sends of one request id are not deduplicated.
- **CORE-08 and later activation paths** (provider registry, CORE-12
  contributions) must start workers through `StartExtensionInstance` /
  `check_startable` so safe mode, quarantine, claims and pinning apply; a
  provider run should pass its run/turn id as `requestId`.
- **CORE-12:** the protected recovery entry is Settings → Extensions; custom
  sidebars or layouts must not hide it. **CORE-05:** CLI/SDK wrappers for the
  four new methods. **CORE-09–11:** first-party shipped packages are outside
  the device package store and outside safe mode.
- **CI:** the engine lifecycle suite needs native process control and
  `python3`; it is not in the hosted workflow.
