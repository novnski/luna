# Parallel audit infrastructure acceptance

Write and exercise these failure cases before changing the wrappers. The
repeatable subprocess harness is `scripts/verify-qa-infrastructure.sh`; it
retains logs under `target/qa/qa-infrastructure.*` and removes its own fixtures.
It does not compile Luna or operate the main app.

1. Reject partial identities, unsafe names/identifiers, the main IPC port,
   nonnumeric ports/revisions, shared or escaping data paths, and identity
   collisions before building or requesting any quit.
2. Preserve the existing bundle and running process after a failed build,
   failed signing, refused graceful quit, occupied port, or failed launch.
   Quit by the exact process previously matched to the selected bundle.
3. Serialize two build subprocesses across linked Git worktrees; reject a
   start without its growth reservation, reject limits above the operating
   ceiling, stop a growing child before the configured threshold, release
   the lock after failure/signals, and preserve child exit codes.
4. Count the primary target, linked targets and shared mbx store once per
   canonical path. Never delete artifacts, replace target directories, or
   modify global Cargo configuration.
5. Real macOS acceptance: build and refresh `LUNA QA Infrastructure`, bundle
   ID `com.novnski.luna.qa.infrastructure`, port `59785`, worktree-local data.
   Record build duration, exact PID/executable, plist identity, signature,
   IPC ownership, storage before/after and unchanged main PID/bundle hash.

The harness uses disposable Git worktrees and command-boundary fault
injection for destructive/failure paths. It is an integration check of the
actual scripts, not a replacement for the final real signed-app acceptance.
The real app is kept available after acceptance; only disposable fixtures
are removed. No unit tests are added after implementation.

## Worker setup

After the coordinator merges this infrastructure, incorporate that commit
into each audit branch before any build. Run all build/check/test/clippy
commands with `scripts/cargo.sh` and `--locked`. Exit 75 requires coordination
with the current lock owner or storage cleanup, not a cache-mode retry.

| Checkout | App name | Bundle ID suffix | Port |
| --- | --- | --- | --- |
| e577 | LUNA QA Providers | providers | 59781 |
| c906 | LUNA QA Tasks | tasks | 59782 |
| abab | LUNA QA Workspace | workspace | 59783 |
| 1e48 | LUNA QA History | history | 59784 |
| b1c8 | LUNA QA Infrastructure | infrastructure | 59785 |
| audit-integration | LUNA QA Integration | integration | 59786 |
| sidebar feature worktree | LUNA QA Sidebar | sidebar | 59787 |

Example, from the provider worktree root (substitute its row for each peer):

```bash
export LUNA_DEV_APP_NAME='LUNA QA Providers'
export LUNA_DEV_BUNDLE_ID='com.novnski.luna.qa.providers'
export LUNA_DEV_IPC_PORT=59781
export LUNA_DEV_DATA_DIR="$PWD/target/luna-qa-data/providers"
python3 scripts/build-budget.py --status
scripts/cargo.sh check --locked -p luna-harness
scripts/run-macos-dev.sh --refresh
codesign --verify --deep --strict "target/luna-macos-dev/$LUNA_DEV_APP_NAME.app"
plutil -p "target/luna-macos-dev/$LUNA_DEV_APP_NAME.app/Contents/Info.plist"
lsof -nP -iTCP:"$LUNA_DEV_IPC_PORT" -sTCP:LISTEN
ps -p <listener-pid> -o pid=,command=
```

The four QA identity overrides above remain required. A QA bundle also sets
the runtime `LUNA_WORKTREES_DIR` to `$LUNA_DEV_DATA_DIR/worktrees` in its signed
plist and LaunchServices invocation, so a synthetic New worktree does not land
under the user's `~/.luna/worktrees`. An optional `LUNA_DEV_WORKTREES_DIR` can
choose another non-symlink directory **inside that same QA data directory**;
the wrapper rejects an empty, external, shared, or non-owned path before build.
The primary LUNA Dev bundle does not set this QA-only worktree override, and
production Repos keeps its user-facing default. A QA setup failure can retain
its Git checkout for inspection; use the recorded checkout path and Git
registration for recovery, never a broad filesystem cleanup.

Confirm the listener PID's executable is the exact chosen worktree bundle.
Record the refresh duration from its output and retain `last-build.log`.
The primary app retains `LUNA Dev`, `com.novnski.luna.dev`, port 59777 and
its existing data. A QA bundle uses the selected name throughout signing,
replacement and rollback; graceful quit targets its exact executable/PID.
QA bundles omit the release/dev URL schemes to avoid deep-link registration
collisions. A stopped packaged peer still reserves its identity, port and
data path. Assign distinct settings before any parallel packaging begins;
the linked-bundle collision scan is not a global atomic identity registry.

`target/luna-macos-dev/.refresh-lock` prevents competing operations in the
same checkout. After an uncatchable kill, inspect its PID and remove only
that stale lock directory when no refresh remains. Never force-quit the app
or remove data to release it. The default primary app invocation and bundle
path remain unchanged.
