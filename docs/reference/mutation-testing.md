# Mutation testing

[cargo-mutants](https://github.com/sourcefrog/cargo-mutants) changes Rust expressions
in a temporary source copy, then runs existing tests to find bugs they miss.
LUNA pins **27.1.0** and uses the existing build lock and disk budget for the
entire campaign, including installation. No product version bump is needed for
this test tooling.

## First run

```sh
scripts/cargo.sh mutants --install
scripts/cargo.sh mutants --list
scripts/cargo.sh mutants
```

The default is a small campaign against `luna-proto`'s context-control contract.
Surviving mutations make it fail and are findings to review, not an installation
failure. It is not a workspace coverage claim. Installation uses a local
`target/mutants/tool/` prefix, not the global Cargo bin directory. After a
successful installation, `target/mutants/tool-build/` is disposable; remove only
that directory once no installer is using it.

## Select the behavior and its tests

```sh
# Whole package (preview its size first).
scripts/cargo.sh mutants --list -p luna-proto
scripts/cargo.sh mutants -p luna-proto -f crates/proto/src/context_control.rs

# Drive mutations through the integration suite that owns the contract.
scripts/cargo.sh mutants -p luna-proto -f crates/proto/src/context_control.rs \
  --test-package luna-engine --test-target task_commands

# Restrict to changed lines; write the diff outside the source tree.
git diff --no-ext-diff --unified=0 origin/main > /tmp/luna-mutants.diff
scripts/cargo.sh mutants -p luna-proto --in-diff /tmp/luna-mutants.diff
```

Use an existing test target for your area (`--test-target` maps to `cargo test
--test`). Without `--test-package`, cargo-mutants runs tests in the mutated
package only: downstream integration coverage is **not** included automatically.
Use the [test-audit authoring gate](../../.codex/skills/test-audit/SKILL.md) before
adding coverage for a survivor. Review equivalent mutations and select the
strongest behavioral owner; do not add tests merely to improve a score.

`--timeout` (default 60 seconds) and
`--build-timeout` (default 300 seconds) bound each phase, including the baseline.
One mutation runs at a time, in stable order. Broad packages can still take a
long time; list and narrow first. Vendor code, test fixtures, and examples are
excluded from mutation. Existing tests remain enabled. Platform-dependent
packages still require their normal SDKs and system libraries.

## Evidence and failures

Every invocation prints a unique `target/mutants/runs/<timestamp>/` directory:

- `run.json`: exact command, tool version, revision, dirty-file status, duration,
  and exit code, plus the guarded wrapper invocation to repeat. An interrupted
  run may have a null exit code; it is incomplete.
- `console.log`: build admission accounting and tool output (JSON candidates
  for `--list`).
- `mutants.out/`: upstream outcomes JSON, mutation diffs, caught/missed/timeout
  lists, and per-mutation build/test logs for executed campaigns.

The unmutated baseline must pass before mutations run. Exit codes propagate:
`0` means all tested viable mutants were caught; `2` means survivors; `3` means
timeouts; `4` means a failing baseline. Guard exit `75` means busy or insufficient
disk budget: coordinate before retrying. The wrapper rejects a zero-candidate
execution with exit `1`; `--list` can still show an empty selection.

Use the wrapper, not bare `cargo mutants` or `--in-place`. Source copies and
temporary test data stay inside `target/mutants/`, and builds reuse `target/ci/`.
The wrapper holds the shared guard once around the external tool; its nested
Cargo commands use plain Cargo (mbx does not wrap this campaign). It does not
alter the global Cargo installation or cache policy. Normal completion removes
scratch copies. After a forced termination, inspect any `scratch-*` directory
and confirm no campaign uses it before removing it. Preserve reports and shared
build caches.

## CI

Run the manual [Mutation testing workflow](../../.github/workflows/mutation-testing.yml)
with a package/file and optional test owner/target. It has a 15-minute job cap
and uploads reports even when mutations survive or tests fail. It is separate
from required PR checks and adds no work to the automatic merge path. Linux
runners support portable core targets; desktop targets need a platform-specific
environment. A cancelled or timed-out campaign is incomplete.

## Integration acceptance

Verify at the real command boundary, keeping the reports:

1. Install the pinned executable and list the default candidates.
2. Run the default campaign; require a passing baseline, nonzero candidate count,
   and caught mutations. Compare source hashes before/after.
3. Run a bounded known-surviving mutation and verify exit `2` plus a missed report.
4. Supply a nonexistent integration target and verify baseline failure (`4`).
5. Hold the build lock and verify admission fails with `75` without starting a
   build. Verify empty selections fail with `1` and interruption stops the
   campaign before scratch cleanup. Check workflow syntax and `git diff --check`.

These checks protect real execution, source preservation, failure propagation,
and guard admission. No unit tests or test-only application hooks are needed.
