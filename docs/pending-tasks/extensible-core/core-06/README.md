# CORE-06: extension manifests, local packages and explicit grants

Baseline: `3c959fb3` (13 October 2026). Live status belongs to
`pending_tasks.py`. This file is the contract, the failure cases (written
before the code), the criteria mapping and the evidence plan.

## Rebased against current main

What already existed and is reused:
- **CORE-01:** `ExtensionId`, `ContributionId` and `NamespacedId` in
  `luna-proto::contract`, and the operation catalog, where a new row is
  `localOnly` until it names a scope.
- **CORE-03:** catalogued `Command`s are admitted through `CommandAdmission`.
  Device-root writers take a `DeviceWriteHandle` permit.
- **CORE-04:** provider identity and the registry. Built-in enablement stays in
  `harness-prefs.json`.
- **CORE-00:** the package format decision. A local `.luna-plugin` directory
  holds a version-1 JSON manifest plus pinned files and content hashes.
  Install, review and enable are distinct, and nothing enables itself.

What was missing: any package format validator, package store, enablement
state or grant record, and any route to inspect or manage them. The test-audit
gate found no existing owner, so one new E2E owner was added.

## Contract

### Manifest (`luna-plugin.json`, version 1)

Types live in `crates/proto/src/extension_packages.rs`. Unknown fields are
refused everywhere, so an install hook is an error, never silently ignored.

| Field | Rule |
| --- | --- |
| `id` | `ExtensionId` (CORE-01 syntax). The `luna` and `luna.*` namespaces are reserved for LUNA's own packages. |
| `version` | Strict `MAJOR.MINOR.PATCH`. An `id` and `version` pair is immutable: different bytes under the same pair are refused. |
| `api` | `{major, minMinor}`. Accepted when the major equals this host's extension API (1.0) and `minMinor` is at most its minor. |
| `runtime` | `data` (nothing executes); `wasm` (`module`, plus the portable profile in `protocol`, 1.0, CORE-00: core modules, no WASI); or `native` (`entries` by `os-arch` key, plus the worker protocol in `protocol`, 1.0). A native package must name an entry for this device. |
| `platforms` | Optional `os-arch` allow-list (`macos-aarch64`). |
| `dependencies` | `{id, minVersion, maxVersion?}`, where `maxVersion` is exclusive. No self-dependency and no duplicates. |
| `contributions` | `{id, kind, title?}`. The id must be the package id or `<package id>.<name>`, so two packages cannot collide. Kinds: `provider`, `command`, `theme`, `settingsSection`, `sidebarSection`, `panel`, `renderer`. A `provider` needs a wasm or native runtime. |
| `capabilities` | `{id, required (default true), reason?}`, drawn from the host vocabulary (`EXTENSION_CAPABILITIES`). Every native package also implicitly requires `luna.capability/native-code`. |
| `settings` / `data` | Settings: a listed JSON Schema file of at most 64 KiB, plus its `schemaVersion`. Data: the data layout's `schemaVersion`, for namespaced migrations. |
| `files` | Every file except the manifest, each with `path`, `size` and `sha256`. A path is canonical, relative and `/`-separated: no `.`, `..`, absolute, `\` or `:` segments, at most 240 bytes and at most 16 levels deep. Spellings must also differ when case is ignored. Limits: 4,096 files, 256 MiB in total, and a 256 KiB manifest. |

**Package hash** (the execution identity) is the SHA-256 of the exact manifest
bytes. The manifest pins every file's hash, so the package hash covers every
byte. Ids never carry the hash; the hash is a separate execution identity.

### Scopes

| Scope | Where | Notes |
| --- | --- | --- |
| Installation (device) | `{data dir}/extensions/`: `packages/<hash>/` holds read-only trees, `installed.json` is the index, and `staging/` holds installs in progress | Outside the app bundle. Created lazily on the first install. |
| Enablement and grants (profile) | `{profile store root}/extension-grants.json` | Each record is `{packageHash, version, grants, enabledAtMs}`. The file is bound to the profile id (`hash(device, scope, org, user)`). |
| Settings values (profile) | Reserved: `{store root}/extensions/settings/<id>` | Specified only; CORE-12 or later creates them. Settings are a preference, never a grant. |
| Package data (profile) | Reserved: `{store root}/extensions/data/<id>/` | Specified only; migrations are CORE-19. |

The grants file is outside every synced document and outside the backup
allow-list. A copy that arrives in another profile, or on another device, is
ignored there: the profile id no longer matches.

### Lifecycle (install is separate from execution)

The states run stage → verify → installed (disabled) → enabled. Activation is
CORE-07.

**Installing** a package directory or a `.tar`/`.tar.gz` archive:
1. Copy it into `staging/<uuid>/tree`. Only regular files and directories are
   accepted. Any link, special file, `..`, absolute or non-canonical name is
   refused before anything is written for it. The copy never follows a link
   (`O_NOFOLLOW`).
2. Read and validate the manifest from the staged copy.
3. Check the exact file set and every byte against the manifest.
4. Check compatibility, version immutability and the dependency closure
   against the installed index.
5. Make the tree read-only, with native entries also executable.
6. Rename it to `packages/<hash>`, then publish the index (temp file, fsync,
   rename).

At no point does installation spawn, load or open package code for execution.
Identical bytes reinstall idempotently.

**Recovery** runs when the engine opens. It removes all staging, index temp
files, grant temp files, and every package directory the index does not list.
With an unreadable index, packages are left untouched. Recovery never fails
engine assembly: a broken store refuses calls and the core keeps running.

**Enabling** requires every one of these:
- the exact installed hash, whose `extensionId` matches;
- an exact grant set: every capability requested by the package, all required
  ones, and no duplicates;
- every file re-hashed and matching;
- each dependency enabled in this profile, at a version in range;
- no other version of the same id enabled.

**Disabling** refuses while an enabled dependent needs the package. A package
whose files changed can still be disabled, but never enabled. Grants are
written atomically (temp file, fsync, rename).

### Protected inspection and management

All four methods are catalog rows with kind as shown, `local` target and
`localOnly` authority:

| Method | Kind |
| --- | --- |
| `ListExtensions` | query |
| `InspectExtension {packageHash}` (re-hashes every file) | query |
| `InstallExtension {path, expectedPackageHash?}` | command |
| `SetExtensionEnabled {extensionId, packageHash, enabled, grants}` | command |

- They are served only by the engine's local service (in-process app and owner
  IPC). A remote device session is refused by the catalog.
- No agent tool calls them.
- A forged field such as `confirmed: true` is a `bad params` error.

The UI route is **Settings → Extensions**: a list, Install…, a review dialog
(hash, trust warning, capability switches) and Disable.

## Failure contract (written before the code)

Each case maps to its owner test in the table below. The test file is
`crates/engine/tests/extension_packages.rs`.

| ID | Failure | Defence |
| --- | --- | --- |
| F1 | A `..` or absolute archive entry writes outside the store | Only `Normal` components are accepted; refused before any write |
| F2 | A symlink, hard link or special file brings outside bytes in, even with a matching hash | Any non-file, non-directory entry is refused; `O_NOFOLLOW` |
| F3 | A tampered file, unlisted extra or missing file is accepted | Exact file set, plus size and SHA-256 of the staged bytes |
| F4 | Installing runs code, for example through an install hook | No spawn path; unknown manifest fields are refused |
| F5 | A local package impersonates `luna.provider/...` | Reserved namespace |
| F6 | An incompatible API, platform or runtime installs | Version and platform gates |
| F7 | A required dependency that is missing or out of range installs or enables | Install checks the installed versions; enable checks the profile's enabled versions |
| F8 | Two versions are enabled at once; a dependency is disabled under a dependent; a version is rewritten in place | One enabled version per id; dependents are checked; versions are immutable |
| F9 | Crash residue becomes an installed or enabled package | Recovery on open; the index and grants are temp-file-and-rename |
| F10 | Enable accepts missing required, unrequested or forged grants, or a stale hash | Exact hash, exact set, no unknown fields |
| F11 | Grants leak to another profile, including through a copied file | Profile-scoped file bound to the profile id |
| F12 | Grants are lost on restart | Durable write; readback after reopen |
| F13 | A package whose files changed after install is enabled again | A full re-hash before enabling |
| F14 | A remote session manages packages | `localOnly` catalog rows; the existing `remote_auth` owner covers the rule |

| Owner test | Cases |
| --- | --- |
| `malformed_and_incompatible_packages_are_refused_without_installing` | F1–F7 (13 real packages) |
| `install_is_inert_and_grants_bind_exact_package_and_profile` | F4, F10–F13 |
| `dependencies_must_be_installed_and_enabled_in_range` | F7, F8 |
| `crash_residue_between_stage_and_commit_never_installs_or_grants` | F9 |

These risks are **not tested**:
- size and count limits (they would need very large inputs);
- the `O_NOFOLLOW` race (a swap between the metadata check and the open);
- Windows, because the suite is `#![cfg(unix)]`.

## Criteria mapping

- **contract:** this file and `extension_packages.rs` in proto, covering ids,
  hashes, API, platform, runtime, dependencies, the contribution namespace,
  the settings, data and trust scopes, and installation kept separate from
  execution.
- **delivery:** `crates/engine/src/extensions{.rs,/manifest.rs,/store.rs,/grants.rs}`,
  the RPC handlers in `crates/engine/src/rpc/extensions.rs`, and the
  Settings → Extensions page in `crates/ui/src/settings/extensions.rs`.
- **verification:** E08 is the four E2E tests above. They use real package
  bytes in temp directories and read the installed tree, the index and the
  grants file back independently. The package hash is recomputed from the
  published rule.
- **handoff:** the orchestrator's build, E2E, lint, review, LUNA Dev refresh
  and delivery.

## Commands (orchestrator)

```sh
scripts/cargo.sh check --locked -p luna-proto -p luna-rpc -p luna-engine
scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract   # regenerate operations.json (+4 rows)
scripts/cargo.sh test --locked -p luna-rpc --features schema --test contract
scripts/cargo.sh test --locked -p luna-rpc --lib remote_auth::tests::policy_inventory_is_complete_and_unknown_fails_closed
LUNA_CORE06_EVIDENCE_DIR="$PWD/target/qa/core06/$(date -u +%Y%m%dT%H%M%SZ)" \
  CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test extension_packages -- --nocapture
scripts/cargo.sh check --locked -p luna-ui
scripts/cargo.sh check --locked -p luna-ui --features appshots-fixture --examples   # settings_gallery.rs arm
scripts/cargo.sh test --locked -p luna-ui --lib settings_entry_and_fixture_routes -- --test-threads=1
scripts/cargo.sh clippy --locked -p luna-proto -p luna-rpc -p luna-engine -p luna-ui --all-targets
```

## Native QA (LUNA Dev, local data only)

Make a data package and a native package with a marker worker:

```sh
agent-python -I - <<'PY'
import hashlib, json, os, sys
root = "/tmp/core06-qa"; os.makedirs(root + "/notes/bin", exist_ok=True)
def pkg(d, m, files):
    os.makedirs(d, exist_ok=True)
    for p, b in files.items():
        os.makedirs(os.path.dirname(os.path.join(d, p)) or d, exist_ok=True)
        open(os.path.join(d, p), "wb").write(b)
    m["files"] = [{"path": p, "size": len(b), "sha256": hashlib.sha256(b).hexdigest()} for p, b in files.items()]
    raw = json.dumps(m, indent=2).encode(); open(os.path.join(d, "luna-plugin.json"), "wb").write(raw)
    print(d, hashlib.sha256(raw).hexdigest())
base = {"manifestVersion": 1, "version": "1.0.0", "displayName": "QA notes", "publisher": "CORE-06 QA",
        "api": {"major": 1, "minMinor": 0}, "runtime": {"kind": "data"}}
pkg(root + "/notes", dict(base, id="core06.qa/notes"), {"notes.txt": b"hello\n"})
plat = {"darwin": "macos"}.get(sys.platform, sys.platform) + "-" + os.uname().machine.replace("arm64", "aarch64")
native = dict(base, id="core06.qa/echo", displayName="QA echo",
              runtime={"kind": "native", "protocol": {"major": 1, "minMinor": 0}, "entries": {plat: "bin/worker"}},
              capabilities=[{"id": "luna.capability/provider.register"}, {"id": "luna.capability/network", "required": False}])
pkg(root + "/echo", native, {"bin/worker": b"#!/bin/sh\necho ran > /tmp/core06-qa/worker-ran\n"})
PY
```

Then, in LUNA Dev:
1. Open Settings → Extensions. Choose Install… and pick `/tmp/core06-qa/echo`.
   - The row shows QA echo, v1.0.0, Native code, the short hash printed above
     and Off.
   - `/tmp/core06-qa/worker-ran` does not exist.
2. Choose Review and enable.
   - The dialog shows the full hash and the native-code warning.
   - The native-code and provider capabilities are locked on; network is off.
3. Choose Enable. The row shows Enabled.
   - Read back `<data dir>/<profile store root>/extension-grants.json`. It
     holds the exact hash and the two grants.
4. Quit gracefully and relaunch. The package is still Enabled and the marker
   is still absent.
5. Disable it, then install `/tmp/core06-qa/notes`. Enable it with no
   capabilities and check the row.
6. Optional: append a byte to the installed `bin/worker` under
   `<data dir>/extensions/packages/<hash>/` (`chmod u+w` first). Revisit the
   page: Review and enable is refused with a "failed verification" error.
7. Capture screenshots and accessibility labels for Settings → Extensions and
   the review dialog.

## Limitations and hand-offs

- **CORE-05:**
  - CLI and SDK exposure (`luna extension list/install/enable/inspect`) should
    wrap these four methods.
  - The proto request and view types are ready for the TypeScript SDK.
- **CORE-07:**
  - Activation, the worker supervisor and broker enforcement of the
    capability vocabulary.
  - Before every start, re-verify the hash and refuse unless the profile
    grant names that exact hash.
- **CORE-18:**
  - A host-owned confirmation receipt for agent-built candidates.
  - Today any owner-IPC client (same user) can call `SetExtensionEnabled`,
    the same authority level as every other owner command. The exact-hash,
    exact-grant binding and the refusal of unknown fields are what is
    enforced now.
- **CORE-19:**
  - Update, rollback, removal and safe mode.
  - Removal must delete grant records for the removed hash.
  - Upgrades must re-request grants. Today a second version can only be
    enabled after the first is disabled, with an explicit new grant set.
- **CORE-09 to CORE-11:**
  - Built-in providers stay in `harness-prefs.json`.
  - Their extraction must seed equivalent grant records, so existing
    enablement and permissions are preserved.
- **Isolated backup roots:** the L17 replaceable-root check refuses an
  isolated root that contains `extensions/`, as it already does for
  `provider-defaults.json`.
- **macOS archives:** `tar` on macOS can add `._*` AppleDouble files. Build
  archives with `COPYFILE_DISABLE=1`, or the unlisted file is refused.
- **Settings gallery:** the fixture `PAGES` list
  (`crates/ui/examples/settings-gallery-fixture.rs`) was already missing
  Connections, and now also lacks Extensions. It was not edited here.
