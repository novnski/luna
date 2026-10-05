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
5. Real macOS acceptance: after the current Luna app saves and quits, build
   and refresh `LUNA QA Infrastructure`, bundle ID
   `com.novnski.luna.qa.infrastructure`, port `59785`, worktree-local data.
   Record build duration, exact PID/executable, plist identity, signature,
   IPC ownership, storage before/after and unchanged primary bundle hash.
6. Refuse a second running LUNA app before building or replacing a bundle,
   preserving the current app and its data. Serialize app operations across
   linked worktrees; a busy app slot must leave the existing bundle intact.

The harness uses disposable Git worktrees and command-boundary fault
injection for destructive/failure paths. It is an integration check of the
actual scripts, not a replacement for the final real signed-app acceptance.
Run only one Luna GUI app at a time. The primary app can remain running during
source review and ordinary Cargo checks; let it save and quit gracefully before
starting a QA app. Never force-quit it to free the slot. Keep the real QA app
available until the coordinator reviews its native evidence. After acceptance,
preserve and hash the evidence, verify no task or process still needs the exact
bundle, quit it gracefully, unregister it from LaunchServices and remove only
that disposable QA bundle. Retain its data and the Cargo cache unless a
separate scoped cleanup has been reviewed. Reopen the primary `LUNA Dev` app
for the user's handoff after the QA app stops. No unit tests are added after
implementation.

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
The same signed plist sets `LUNA_QA_MCP_ISOLATION=1`: provider processes in this
QA app omit saved user MCP servers while retaining LUNA's per-session `luna`
tools. The primary LUNA Dev bundle sets it to `0`, so even an inherited shell
value cannot isolate ordinary user chats. Verify the signed plist and a QA
provider launch before relying on this isolation; the QA data directory alone
does not isolate `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, or Executor.
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

The Git common directory's `luna-app-operation.lock` serializes launch and
refresh operations across linked worktrees; `target/luna-macos-dev/.refresh-lock`
also prevents competing operations in one checkout. The script checks for a
different running Luna app before building and again before replacing a bundle.
Exit 75 means the app slot is busy. After an uncatchable kill, inspect the
recorded PID and remove only a proven stale lock directory when no app
operation remains. Never force-quit an app or remove data to release a lock.
The default primary app invocation and bundle path remain unchanged.
