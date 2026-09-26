# Shared Cargo build cache

LUNA uses [Mr Boxington](https://mr-boxington.jdx.dev/) through
`scripts/cargo.sh`. It can reuse matching dependency compilations across
checkouts, while Cargo keeps the current checkout's incremental workspace
builds. An up-to-date `target/` already avoids compilation; rerunning it is
not evidence of shared-cache hits.

## Install and use

The validated version is **1.18.0**. Install a checksum-verified release binary
using the [official installation guide](https://mr-boxington.jdx.dev/installation),
or build the pinned version with `cargo install mbx --version 1.18.0 --locked`.
On this Mac the verified Apple Silicon binary is installed at
`~/.local/bin/mbx`. The wrapper searches `PATH`, then that location; set
`LUNA_MBX_BIN` to an absolute executable path for a different installation.
It uses the Rust toolchain selected by this repository's `rust-toolchain.toml`.

```bash
scripts/cargo.sh check --locked -p luna-proto
scripts/cargo.sh test --locked -p <changed-package>
scripts/cargo.sh clippy --locked -p <changed-package>
scripts/run-macos-dev.sh --refresh
```

`LUNA_BUILD_CACHE=auto` is the default: use mbx when installed, otherwise use
Cargo with a notice. `LUNA_BUILD_CACHE=1` requires mbx; `LUNA_BUILD_CACHE=0`
uses plain Cargo. The wrapper does not retry failed builds. Non-compilation
commands, including `fmt`, `metadata` and `clean`, go directly to Cargo;
`clean` still requires the scoped procedure in `AGENTS.md`.

The integration is local and project-scoped. It installs no Cargo shim and
does not configure a remote cache. Existing CI jobs retain their current
`Swatinem/rust-cache` configuration. Do not run `mbx setup` or add global mise
wrapping as a prerequisite for this project.

## Policy and disk usage

- `.mbx.toml` preserves Cargo incremental mode, workspace source paths and
  original `OUT_DIR` paths. Incremental compilations remain private and bypass
  the shared cache; dependencies remain eligible. Preserving `OUT_DIR` paths
  reduces cross-checkout reuse for crates that embed those paths.
- `scripts/cargo.sh` disables managed targets and mbx's separate learned
  incremental compilation. It keeps the native linker. LUNA's `target/`,
  packaged apps and dev data stay under LUNA's existing lifecycle rules.
- The shared action store defaults to a **20 GiB collection budget** via
  `MBX_GC_MAX_SIZE`. It is separate from the 50 GiB `target/debug` threshold.
  Automatic collection normally runs after builds at most once per hour;
  the store can temporarily exceed the budget. APFS cloning can reduce
  physical duplication. Hard-link restoration is disabled so plain Cargo
  can overwrite restored files on other filesystems.
- Explicit environment overrides still apply to mbx's workspace policy.
  Use `CARGO_INCREMENTAL=0` for one-off checks as directed in `AGENTS.md`;
  mbx may print a reminder that Cargo incremental compilation is disabled.
  Avoid switching build flags during an ordinary edit loop.
- Storage settings are passed by the wrapper because mbx rejects them in
  a repository-owned `.mbx.toml`. Do not put machine paths or credentials
  in that file. The wrapper does not modify global mbx configuration.

Inspect the cache with `mbx cache stats --json` and explain the last build
with `mbx explain --last`. Preview collection using
`MBX_GC_MAX_SIZE=20GiB mbx gc --dry-run`. The budget applies to the shared
action store, including entries from other projects; coordinate cleanup
with active tasks. Never use `mbx adopt`, `mbx clean` or `mbx cache remove`
on this checkout, because `target/` also contains persistent app data.

## Repeatable verification

Run `scripts/verify-build-cache.sh` (requires Cargo, mbx and jq). It checks
the real `luna-proto` crate into two fresh directories under
`target/.build-cache-e2e.*`, verifies reported restores and identical itoa
metadata, and confirms plain Cargo can consume the restored output.
It also checks missing-tool fallback, strict mode, toolchain forwarding,
Cargo cleanup semantics and failed-build exit codes. These disposable
targets are removed on success or failure.

The verifier prints and retains a unique `target/qa/build-cache.*` directory
containing logs, mbx JSON reports, `summary.json` and `result.txt`. Require
`exit=0` and nonzero restored hits. The first run can already hit an existing
store, so this check proves reuse rather than a cold-build speedup. Run the
macOS refresh separately to verify the full app and its signature/IPC.

Upstream references: [incremental behavior](https://mr-boxington.jdx.dev/incremental),
[managed targets](https://mr-boxington.jdx.dev/managed-targets), and
[configuration](https://mr-boxington.jdx.dev/configuration).

## Validation contract

Before changing this integration, account for these failure modes:

- A missing `mbx` must allow the default build to use Cargo; explicitly
  requiring the cache must fail clearly. Disabling it must use Cargo.
- A failed cached build must preserve its exit status, without silently
  retrying through Cargo. Arguments and `+toolchain` selection must survive.
- The wrapper must not move, symlink, or collect LUNA's `target/`, which
  contains the development app and its persistent data. `clean` must retain
  Cargo's semantics and the existing scoped-cleanup rules.
- Switching to the wrapper must preserve Cargo's incremental policy, native
  linker and literal source paths. Plain Cargo must still be able to write
  restored artifacts on filesystems without copy-on-write cloning.
- Cache verification must use two fresh output directories and observe real
  restored compiler outputs, not merely an up-to-date Cargo build. Keep the
  JSON reports and logs, and remove only the disposable output directories.
- A successful build is not a completed macOS handoff: verify the refreshed
  bundle's process, IPC listener and signature.
