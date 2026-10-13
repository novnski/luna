# AW-09: read-only diagnostics and owned storage accounting

Baseline: `54517c69` (13 October 2026). Live status belongs to
`pending_tasks.py`. This file is the rebase triage, the bounded contract,
the failure cases (written before the code), the criteria mapping and the
evidence plan. Sources: [research README](../README.md),
[opportunities #9](../luna-opportunities.md#ranked-opportunities),
[settings gap matrix](../settings-analysis.md#workflow-and-machine-operation).

## Rebase: every corresponding row against current main

| Research row | Current main (owner) | Verdict |
| --- | --- | --- |
| Opportunity 9 "Read-only diagnostics and storage view" | No Settings destination aggregates process, connection, version, resource or error state; no storage accounting anywhere | **Gap** (this ticket) |
| Settings "Process/resource/log diagnostics" | Per-chat Execution card (`execution_view.rs`) only; engine pid/uptime/resident memory are not read back by any RPC | **Gap**: engine process readback + one read-only page |
| Settings "Storage usage/retention/cleanup preview" | Backups page reports archive sizes; Usage page owns usage scans; nothing measures the data folder | **Gap** for read-only accounting; retention/cleanup **out of scope** (separate reviewed action) |
| Engine identity (device, scope, profile) | `EngineInfo` (`ENGINE_INFO`, held by `EngineHandle`) | Already shipped; **reused**, not re-served |
| App/engine version | `luna_update::current_version()` (Updates page); engine `CARGO_PKG_VERSION` (`DescribeApi`) | Already shipped; app version reused, engine version reported with the process row |
| Provider availability and CLI versions, OpenCode version trust | `LIST_HARNESSES`, `GET_PROVIDER_UPDATES` (Settings → Providers, OC-17 compat detail) | Already shipped; **reused read-only** (non-forced, cached check); actions stay on Providers |
| Viewport ⇄ engine connection, edge posture | `AppState.connection`, `AppState.connectivity` (`WatchConnectivity`) | Already shipped; **reused** |
| Extension runtime resource readback | CORE-07 `LIST_EXTENSION_INSTANCES` (state, pid, resident bytes, faults, quarantine) | Already shipped; **summary reused**, management stays on Extensions |
| Deletion cleanup failures | `GET_DELETION_CLEANUP_SUMMARY` (sidebar notice) | Already shipped; **reused** as a problem row |
| Backups | `GET_BACKUP_STATUS` (Backups page) | Already shipped; storage only measures the marked managed `<data>.backups` folder |
| Usage scans (`usage-scan-v2.json`, 179 MB on the dev profile) | `usage.rs` / Usage page | Owner unchanged; accounted as "Usage scans" |
| Remote authority unavailable | Connections page (`remote_store()` reason) | Already shipped; not duplicated |
| Per-session MCP status near the composer | Provider catalog in Settings → Providers | **Out of scope** (composer/session surface, not diagnostics) |
| Model capability visibility | Per-chat Execution card | Already shipped; out of scope |
| Proposed "System" navigation group | Flat Settings list | **Out of scope** (navigation/registry work); Diagnostics joins the list beside Devices/Backups |

Real native behaviour observed on the dev profile (read-only, `ls`/`du`
of `target/luna-macos-dev/data`): twelve leftover backup staging folders
`.luna-l17-snapshot-*` (~100 MB, `backup.rs` tempdirs left by interrupted
exports), an ownerless `adapters/` folder (878 MB, no current code path),
and the 179 MB usage scan. None was visible in the app before this ticket.

## Contract (required)

**Engine** (`crates/engine/src/diagnostics.rs`, handlers in
`crates/engine/src/rpc/diagnostics.rs`, types in
`crates/proto/src/diagnostics.rs`): two `query`, `unary`, `local`,
`localOnly` catalog rows, served only by the engine's local service (owner
IPC and the in-process app):

- `GetEngineDiagnostics` → `EngineDiagnostics`: engine version, pid,
  start time, uptime, OS/arch, and sampled resources of the engine process
  (resident bytes, user/system CPU, threads; each `null` where the platform
  cannot sample). Identity stays in `EngineInfo`.
- `GetStorageUsage` → `StorageUsage`: one bounded, read-only walk of what
  this engine owns on disk, as categories with scope (`profile`, `device`,
  `shared`), location, state (`complete`, `partial`, `missing`,
  `notScanned`), apparent bytes, files, directories, unreadable entries and a
  path-free first error. Categories: workspace database, run journals,
  attachments, terminal history, extension data, other profile state (this
  profile); turn snapshots, model catalogs, usage scans, extension packages,
  logs, repositories, update downloads, legacy attachments, backup staging,
  settings and identity, unrecognized (this data folder); backup archives
  (`<data>.backups`, only with LUNA's marker) and managed worktrees (shared
  when outside the data folder). Other profiles' folders are listed as
  exclusions with a count and never walked.
- Bounds: the walk runs on the blocking pool, one at a time per engine,
  never follows links, stops at 1,500,000 entries or 15 s and then reports
  `complete: false` with the stop reason; small profile categories are
  measured first and worktrees last.
- Read-only: only `canonicalize`, `symlink_metadata` and `read_dir`; no
  file content is opened; nothing is created, touched or deleted.

**UI** (`crates/ui/src/settings/diagnostics.rs`, Settings → Diagnostics):
one page whose sections load independently from the owners above: *This
app and engine* (version, in-process vs separate engine, pid, uptime,
resources, profile scope, connection), *Providers* (installed/enabled,
version, update, compat detail), *Extensions* (instance and quarantine
summary), *Problems* (connection failure, edge failure, deletion cleanup
failures, extension faults, storage read errors) and *Storage* (total,
categories, exclusions, stop reason). Each section renders loading, error
or partial state without hiding the others. Refresh re-reads; there is **no
cleanup, delete or reveal control**, only a note that cleanup is a separate
reviewed action. The page re-reads when the engine is replaced or
re-attached, and replies from a previous engine are dropped (generation
fence).

**Redaction:** locations inside the data folder are relative; outside it
they are `~`-abbreviated; unrecognized items list top-level names only (at
most 20); errors carry an I/O kind, never a path; no file content, token,
credential or environment value is read or returned.

## Failure contract (written before the code)

| ID | Failure | Defence | Owner test |
| --- | --- | --- | --- |
| S1 | The scan writes: creates a missing folder, touches a file or changes permissions | Only metadata/listing calls; tree snapshot before/after; absent folders stay absent | E2E `storage_usage_matches_an_independent_walk_and_never_writes` |
| S2 | A symlink leads the walk outside the data folder or into a loop | Links are never followed; a link counts as one entry of its own size | same (link to a 100 KB outside file) |
| S3 | A missing folder fails the scan | Category `missing`, not an error | same (`repos`, `updates`) |
| S4 | An unreadable folder fails the scan, or a partial size is shown as complete | Per-entry error tally, category `partial`, path-free reason | same (`logs/sealed`, mode 000) |
| S5 | A huge tree blocks the engine or the UI | Blocking pool, single flight, entry and time budget, `complete: false` + stop; UI says "at least" | code bound; UI partial rendering (`diagnostics_sections_load_independently_and_render_partial_storage`) |
| S6 | Another profile's data is measured or added to totals | Exclusions with counts only; never walked | E2E (`profiles/other-local`, `orgs/…/…` → 2 exclusions, never unrecognized) |
| S7 | A folder that isn't LUNA's is counted as backups | `.luna-l17-isolated-backups` marker required | E2E (marked sibling measured exactly) |
| S8 | The profile or worktree root inside the data folder is counted twice | Skip set during the device walk | E2E (profile container never unrecognized; totals = sum of categories) |
| S9 | Paths outside the profile, file contents or secrets leak | No content reads; relative / `~` locations; kind-only errors | E2E (error text has no path) |
| S10 | A remote session or a forwarded call reads diagnostics | `localOnly` rows; local service only; foreign `targetDeviceId` refused | E2E `engine_diagnostics_report_this_process_and_stay_local` (paired session with every read scope is denied) |
| S11 | The page offers cleanup or sends a command | Page calls read methods only; no destructive control | every UI test (no catalogued command in the RPC trace; each owner read once) |
| P1 | The UI's own process is shown as a separate engine's, or vice versa | Engine reports its own pid; UI labels in-process vs separate | E2E pid; UI engine row |
| P2 | Unsupported resource sampling fails the call | Optional fields, "not available" | E2E (`Some` on macOS/Linux) |
| U1 | One failing owner hides the page | Independent `Loadable` per section | UI `diagnostics_sections_fail_independently` |
| U2 | Loading blocks the UI | Async RPC in `cx.spawn`; walk off the UI thread | UI `diagnostics_sections_load_independently_and_render_partial_storage` (held replies) |
| U3 | A late reply from a previous engine overwrites the current one, or the page keeps showing the old engine | The page re-reads on every engine generation change; a refresh drops in-flight loads; generation fence on every reply | UI `late_storage_reply_from_previous_engine_is_dropped` |

## Required vs optional follow-ups

Required here: the two RPCs, the page, the tests above, this document.

Optional, separate tickets (not started): reviewed cleanup preview with
candidate ownership, active-use checks and an exact preview (begin with
backup staging leftovers and ownerless folders); a logs view / reveal
action; diagnostics of another device (device-addressable rows plus remote
scope review); allocated-size and hard-link/clone-aware accounting;
per-worktree breakdown with active-task attribution; settings search and a
"System" navigation group (registry work); per-session MCP status in the
composer.

## Criteria mapping

- **rebase:** the triage table above (shipped owners reused, gaps named,
  out-of-scope rows explicit) plus the native observation.
- **scope:** the contract, failure table and follow-up split above, written
  before implementation; passing owners (Providers, Extensions, Backups,
  Usage, Updates, Connections) are untouched.
- **verification:** `crates/engine/tests/diagnostics_storage.rs` (real
  disposable profile, independent walk, no-write snapshot, local-only
  refusal) and `crates/ui/src/settings/diagnostics_tests.rs` (rendered
  loading/error/partial/fenced states over held memory RPC, read-only trace),
  plus the native QA below. No second subsystem: every non-storage value
  comes from its existing owner.
- **handoff:** the orchestrator runs the commands, QA and delivery below.

## Files

Owned: `crates/proto/src/diagnostics.rs`, `crates/engine/src/diagnostics.rs`,
`crates/engine/src/rpc/diagnostics.rs`, `crates/engine/tests/diagnostics_storage.rs`,
`crates/ui/src/settings/diagnostics.rs`, `crates/ui/src/settings/diagnostics_tests.rs`,
this file.

Shared, additive hunks: `crates/proto/src/lib.rs` (module + re-export);
`crates/rpc/src/lib.rs` (2 constants + `ALL`); `crates/rpc/src/operations.rs`
and `crates/rpc/contract/v1/operations.json` (2 rows after
`StageBackupRestore`); `crates/engine/src/lib.rs` (module, `EngineCore`
field, construction, local-service attach); `crates/engine/src/rpc.rs`
(submodule, field, `None` init, route); `crates/ui/src/settings.rs`
(module); `crates/ui/src/shell.rs` (section variant, `ALL`, fixture route,
label, icon, page field/init, refresh on visit, outlet);
`crates/ui/src/shell/settings_gallery.rs` (mounted arm);
`.github/workflows/ui-tests.yml` (one engine test step).

## Commands (orchestrator)

```sh
python3 scripts/build-budget.py --status
scripts/cargo.sh check --locked -p luna-proto -p luna-rpc -p luna-engine
scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract   # must leave operations.json unchanged (+2 rows added by hand)
scripts/cargo.sh test --locked -p luna-rpc --features schema --test contract
scripts/cargo.sh test --locked -p luna-rpc --lib remote_auth::tests::policy_inventory_is_complete_and_unknown_fails_closed
EVIDENCE="$PWD/target/qa/aw09/$(date -u +%Y%m%dT%H%M%SZ)"
LUNA_AW09_EVIDENCE_DIR="$EVIDENCE" CARGO_INCREMENTAL=0 \
  scripts/cargo.sh test --locked -p luna-engine --test diagnostics_storage -- --nocapture
LUNA_EPOCH_QA_ARTIFACTS="$EVIDENCE/ui" \
  scripts/cargo.sh test --locked -p luna-ui --lib settings::diagnostics::tests -- --nocapture
scripts/cargo.sh clippy --locked -p luna-proto -p luna-rpc -p luna-engine -p luna-ui --all-targets
scripts/run-macos-dev.sh --refresh
(cd "$EVIDENCE" && find . -type f -print0 | sort -z | xargs -0 shasum -a 256 > SHA256SUMS)
```

The engine suite is Unix-only (symlink and mode-000 fixtures). Run as a
normal user: as root the mode-000 folder stays readable and the test prints
`SKIPPED unreadable` for that one assertion. It writes
`storage-usage.json`, `independent-walk.json`, `engine-diagnostics.json`
and `remote-refusals.json` to `$LUNA_AW09_EVIDENCE_DIR` when set. Turn
snapshots are checked as missing rather than planted: assembly sweeps other
runs' snapshot stores in the background, so a planted store could vanish
mid-test. The UI traces land in
`$LUNA_EPOCH_QA_ARTIFACTS` (`aw09-*.json`).

## Native QA (LUNA Dev, read-only on the primary data)

1. After the refresh, open Settings → Diagnostics. *This app and engine*
   shows "In this app", a pid equal to
   `pgrep -f 'LUNA Dev.app/Contents/MacOS'`, the same version as Settings →
   Updates, uptime, resident memory and the Local profile scope.
2. *Providers* lists the same installed versions as Settings → Providers
   (no update is started); *Extensions* matches Settings → Extensions.
3. Record `ls -A target/luna-macos-dev/data > /tmp/aw09-before.txt`, press
   Refresh twice, then `ls -A … > /tmp/aw09-after.txt`: identical names (no
   `turn-snapshots`, `updates` or `repos` folder appears).
4. *Storage* lists Backup staging (12 leftover folders), Unrecognized
   (`adapters`) and Usage scans. Compare with an independent walk:

   ```sh
   python3 - target/luna-macos-dev/data <<'PY'
   import os, sys
   root = sys.argv[1]
   def walk(path):
       total = 0
       for base, dirs, files in os.walk(path, followlinks=False):
           for name in files + [d for d in dirs if os.path.islink(os.path.join(base, d))]:
               total += os.lstat(os.path.join(base, name)).st_size
       return total
   for name in ['logs', 'model-cache', 'adapters']:
       print(name, walk(os.path.join(root, name)))
   print('staging', sum(walk(os.path.join(root, n)) for n in os.listdir(root) if n.startswith('.luna-l17-snapshot-')))
   print('usage', sum(os.lstat(os.path.join(root, n)).st_size for n in os.listdir(root) if n.startswith('usage-')))
   PY
   ```

   Model catalogs, Backup staging and Unrecognized match exactly; Logs and
   Usage scans may differ by what the running app wrote between the two
   reads.
5. No Delete, Clean up or Reveal button exists; the cleanup note says it is
   a separate reviewed action. Screenshot the page (light and dark) into the
   evidence folder.

## Limitations

- Sizes are apparent sizes (`du -A`); hard links and APFS clones are
  counted once per path, sparse files at their logical size.
- The default worktree root (`~/.luna/worktrees`) is shared by every LUNA
  data folder that does not override it; it is labelled shared, not
  attributed per profile.
- Backup archives in a custom destination of a copy-isolated root are not
  measured; the Backups page reports their sizes.
- The walk budget is fixed; a stopped scan reports lower bounds.
- Diagnostics read only the attached engine; other devices are a follow-up.
