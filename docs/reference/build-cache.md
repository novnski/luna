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

The mbx integration is local and project-scoped. It installs no Cargo shim and
does not configure a remote cache on this Mac. CI uses the separate compiler
cache described below. Do not run `mbx setup` or add global mise wrapping as a
prerequisite for this project.

## GitHub Actions

[CI](../../.github/workflows/ci.yml) is the automatic pull-request and main-push
workflow. It selects affected suites from changed paths, runs them in parallel,
and exposes one `Required checks` result. Documentation-only changes skip the
Rust graph. Shared manifests, toolchains and CI setup changes exercise all Rust
suites. A failed, cancelled or timed-out selected suite fails the final result.

Parallel jobs have a nine-minute execution ceiling. The final gate independently
enforces **600 seconds for the whole run**, using GitHub's run timestamp and
including queue time (reruns use their attempt start). A timeout, failed suite,
or elapsed time over the budget fails the gate; none count as successful
verification. The measured time is retained in the job summary.

The `ci` Cargo profile keeps development assertions and overflow checks, with
no optimization, debug information or incremental output. It leaves local dev
and release profiles unchanged. The full headless UI library suite compiles once
in its original Linux test environment. Separate macOS/Linux compile checks validate the application platform
contracts without serializing a second ordinary UI library build ahead of the
UI test executable. Changed fixture examples run with their required features
in the macOS compile job; unchanged screenshots and fixture executables stay
out of the merge path. Core contracts, provider fixtures,
engine lifecycle E2Es, preview networking and edge runtime checks have separate
owners. iOS changes compile the Swift app and tests against freshly generated
Rust bindings without booting a simulator; the opt-in `LUNA_IOS_CARGO_PROFILE=ci`
is rejected for Release builds.

The extended engine matrix, native browser screenshots, simulator runtime
suite and performance probes remain available through the manual **Full
platform verification**, **Full preview networking**, and **Core baseline
probes** workflows. They no longer run on every pull request or main push.
Release and TestFlight workflows retain their distribution profiles.

Rust jobs use [the shared setup action](../../.github/actions/rust-ci/action.yml).
It updates stable Rust consistently, then restores a `Swatinem/rust-cache`
dependency archive. Compatible core jobs share `core-ci`; desktop and iOS use
`desktop-ci` and `ios-ci`. The action also keys by OS, architecture, compiler,
environment and dependency manifests. Do not add a commit SHA or individual
job name to a shared group. The engine job saves the largest core graph;
smaller core jobs only restore it so they cannot win the immutable archive save
with a partial dependency set. Only successful main-branch jobs save caches:
PRs read main's cache without duplicate archives or uploads extending the merge
path. Failed/timed-out jobs do not spend cleanup time exporting build output.
Manual/distribution profiles use separate groups.

GitHub caches are reusable, not permanent: branch access restrictions, the
repository storage limit and seven-day idle eviction still apply. Do not raise
paid storage limits as part of ordinary CI work. See [GitHub's cache rules](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching).

For repeatable verification, run every fast suite through `workflow_dispatch`
(or a CI/configuration PR), retain run/job JSON and logs, and measure both cold
and warm runs. Also verify that a documentation-only change skips the matrix
and a failed selected suite cannot produce a green final gate. This is CI
infrastructure only and requires no product version bump or local app refresh.

## Policy and disk usage

- `.mbx.toml` preserves Cargo incremental mode, workspace source paths and
  original `OUT_DIR` paths. Incremental compilations remain private and bypass
  the shared cache; dependencies remain eligible. Preserving `OUT_DIR` paths
  reduces cross-checkout reuse for crates that embed those paths.
- `scripts/cargo.sh` disables managed targets and mbx's separate learned
  incremental compilation. It keeps the native linker. LUNA's `target/`,
  packaged apps and dev data stay under LUNA's existing lifecycle rules.
- The shared action store defaults to a **20 GiB collection budget** via
  `MBX_GC_MAX_SIZE`. The wrapper keeps it fixed at `20GiB` during the shared
  audit; changing it requires a coordinated policy change. It is separate
  from the 70 GiB per-checkout cleanup review threshold.
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

## Shared build admission

`scripts/cargo.sh` runs compilation commands through `scripts/build-budget.py`
(Python 3 standard library, macOS/Linux). It holds an advisory lock in the
repository's common Git directory for the entire subprocess lifetime. All
linked worktrees share that lock, including worktrees added later. A competing
build exits **75** with its owner's PID and checkout; it does not queue or
start another compiler. Ordinary Cargo exit codes and interrupt signals are
preserved. Non-compilation commands still go directly to Cargo.

```bash
python3 scripts/build-budget.py --status
scripts/cargo.sh check --locked -p luna-proto
```

Admission requires current usage plus a **25 GiB growth reservation** to fit
under the **170 GiB operating ceiling**, and that much free disk plus 10 GiB.
Before admission, the guard prunes duplicate `target/debug/incremental/`
directories when that cache exceeds **12 GiB** by conservative accounting.
It keeps the two most recently used directories per crate and anything used
within 24 hours. This may make an older build variant recompile later, but it
does not remove dependency outputs, apps, data, release artifacts, or the
current incremental cache. The guard holds the shared build lock during
cleanup and skips it for QA scratch target directories. Preview the same
selection with `python3 scripts/prune_incremental.py`; the printed sizes use
the guard's accounting and are not a promise of physical disk space recovered.
The guard measures all linked checkout `target/` directories (including apps,
QA artifacts and data) and the entire mbx cache root. This intentionally
includes more than the action store. It samples after each two-second wait
and after child exit, stopping only its own build process group if usage
reaches the ceiling, growth reaches the reservation, or free disk falls below
10 GiB. It sends TERM, waits up to ten seconds, then kills remaining build
processes if necessary. It never cleans files. A budget failure exits 75.

`LUNA_BUILD_BUDGET_GIB` can **lower** 170 and `LUNA_BUILD_RESERVE_GIB` can lower
25 for a bounded check; neither can raise the default. Reservation is also
the build's growth limit, so do not lower it below the expected build size.
Explicit target overrides must resolve inside the current checkout's target;
target symlinks and Cargo config/path overrides are refused. Do not configure
an external intermediate build directory in machine/repository Cargo config.
All peers and main must use the updated wrapper before builds are released.

The absolute policy maximum remains **200 GiB**. The unused 30 GiB provides
headroom for packaging, sampling delay and unrelated activity. This is a
cooperative guard, **not a hard filesystem quota**: a fast writer can grow
between samples, main using an old wrapper or direct Cargo bypasses the lock,
and other projects can write the shared cache. Stop and coordinate if any
uncoordinated writer threatens that headroom. A strict kernel-enforced cap
would require a separately provisioned quota, outside this change.

Accounting charges the larger of logical and allocated bytes per file,
canonicalizes roots but does not follow descendant symlinks, avoids duplicate
canonical roots and same-root hard links,
and charges APFS clones separately. It is deliberately conservative and is
not APFS physical-exclusive usage. The sample is not atomic; files may change
during traversal. Only linked worktree targets and the discovered mbx root
are included, not unrelated clones or Cargo registry/download caches.
The permanent lock file must **never be deleted** to clear a busy build;
the OS releases its advisory lock when the guard exits. Inspect the reported
PID instead. A killed supervisor cannot supervise surviving children, so
inspect processes before retrying after abnormal termination.

One checkout's 70 GiB threshold requests a cleanup review; it does not grant
70 GiB to each peer. Follow `AGENTS.md` for coordinated dry-run/scoped cleanup.
Keep persistent QA data, apps and active artifacts. The shared action store
remains at its original 20 GiB collection setting.

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
