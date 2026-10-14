# Development, daily-use Alpha, and releases

This workflow separates source development from the binary used day to day. An Alpha build is a frozen, identifiable candidate, not a promise that the app is bug-free.

## Two macOS app channels

| Property | LUNA Dev | LUNA Alpha |
| --- | --- | --- |
| Purpose | Coding, tickets, and testing current source | Day-to-day use of one chosen snapshot |
| Bundle | `target/luna-macos-dev/LUNA Dev.app` | `~/Applications/LUNA Alpha.app` |
| Identity | `com.novnski.luna.dev` | `com.novnski.luna.alpha` |
| Data | `target/luna-macos-dev/data` | `~/.luna-alpha` |
| Worktrees | Existing developer worktree configuration | `~/.luna-alpha/worktrees` |
| IPC | `59777` | `59779` |
| Build | Cargo development profile | Optimized Cargo release profile |
| Replacement | Agent refresh after meaningful source work | Only an explicit user request |

Only one LUNA GUI runs at a time. Switching asks the current app to save and quit gracefully. If it declines, the operation stops without force-quitting or swapping a bundle. Unknown QA/installed app identities are left alone and must quit through their own owner.

```sh
# First daily installation: committed, clean main is required.
python3 scripts/macos-app.py install-alpha

# These are the normal channel switches:
python3 scripts/macos-app.py alpha  # open the installed snapshot; no build
python3 scripts/macos-app.py dev    # leave Alpha and run guarded Dev refresh
python3 scripts/macos-app.py status
python3 scripts/macos-app.py quit  # save/quit the known current channel

# Only when the user asks to replace the daily snapshot:
python3 scripts/macos-app.py update-alpha
```

The first snapshot is `0.0.1-alpha.1`. Another explicitly requested snapshot of the same product version increments the Alpha counter (`0.0.1-alpha.2`); a new product version starts its own Alpha counter. The installed app has a separate monotonic bundle build number, full source revision, and build timestamp. About shows the Alpha version and commit. `status` reads its signed build manifest. CLI/engine version output remains the numeric product version; SDK and protocol versions remain independent.

The installer builds all three native runtimes (app, extension host, OpenCode worker), stages and verifies the signature, then switches. Failed compilation/signing leaves current apps intact. Updates create a verified offline data backup using the old Alpha executable before replacement. The previous bundle is retained until the new process and IPC listener are confirmed. If launch fails, the previous bundle is restored without automatically opening it; data is not silently rolled back. If the new process declines to quit during recovery, the previous bundle is retained in the reported staging directory.

Alpha uses its bundled runtimes, independent profile/settings/extensions/worktrees, and a signed `LUNAUpdatePolicy=manual` marker. The updater refuses installation even if the bundle is renamed. Opening Alpha, building Dev, bumping a source version, merging, and pushing `main` do not replace it. A future user-requested Alpha update is a deliberate promotion of a clean, committed source revision.

The workflow does not import Dev history into Alpha or migrate provider credentials. Providers retain their own authentication. Local signing prefers an available Apple Development identity, with ad-hoc signing as a fallback; this is not a notarized public release and permissions may be requested again with ad-hoc signing.

## Product version policy

LUNA uses [SemVer](https://semver.org/): `MAJOR.MINOR.PATCH`, beginning with `0.0.1`. The compatibility surface includes the app's user-visible behavior, CLI, RPC/SDK operations, extension contracts, and saved-data behavior. Version zero communicates initial development; it does not excuse unannounced data loss or broken contracts.

| Change | Product bump | Example |
| --- | --- | --- |
| Compatible bug fix, security fix, or performance improvement without new functionality | Patch | `0.0.1` → `0.0.2` |
| New user-facing functionality | Minor; reset patch | `0.0.2` → `0.1.0` |
| Breaking behavior/API change while below `1.0.0` | Minor, with explicit migration/compatibility notes | `0.1.4` → `0.2.0` |
| Documentation, tests alone, formatting, or refactoring with unchanged behavior | None | Keep current version |
| Declaring the first stable compatibility contract | `1.0.0`, only when explicitly authorized | Not an automatic feature bump |
| Breaking public contract after `1.0.0` | Major; reset minor and patch | `1.2.3` → `2.0.0` |

Agents choose the highest impact once per completed feature/PR, before its app build, and record the result in `CHANGELOG.md`. Do not bump for every edit, every file, or again when landing a bump already made for the same work. This initial workflow establishes the `0.0.1` baseline rather than applying a second feature bump on top of the reset.

```sh
python3 scripts/version.py show
python3 scripts/version.py bump patch --dry-run
python3 scripts/version.py bump patch  # or minor; changes source only
python3 scripts/version.py check --metadata
```

The command updates the root workspace version, only its inheriting package entries in `Cargo.lock`, and all iOS marketing configurations. It preserves dependency versions and independently versioned packages (including `luna-sdk`/`@luna/sdk`), uses locked Cargo metadata to validate the result, and rolls back its own edits if validation fails. SDK/API, persistence-schema, and wire-protocol versions must only change through their owning compatibility workflow; do not reset them to the app version. iOS build numbers are separate and the TestFlight workflow continues to allocate them monotonically.

## Public release flow

1. Complete the version bump, changelog, relevant builds, E2E evidence, and platform checks. Commit, merge, and verify `origin/main` as usual.
2. Run `python3 scripts/version.py release-plan`. It checks clean `main`, product/lock/iOS consistency, exact remote source, and an unused local/remote tag, then prints a plan. It creates no tag and publishes nothing.
3. Only on an explicit publish request, create an annotated `v<product-version>` tag at that verified revision and push that tag over SSH. Never overwrite a tag or an existing release. Check remote tags/releases before doing so.
4. The release workflow validates the numeric tag against the workspace, requires it to be on `main`, and rejects an existing GitHub Release before building. It packages complete desktop runtimes, then publishes signed updater metadata and grouped release notes. `0.x` versions are marked GitHub prereleases and do not become the stable updater's latest release. A manually dispatched workflow builds artifacts without publishing.
5. Verify the published artifacts, signatures, install/restart behavior, and release notes. Public release publication still does not promote this Mac's Alpha installation.

The inherited version reset is a new initial-development line. Existing historical tags/releases, provider versions, independent packages, and user data must not be rewritten to match it. At setup, this repository had no published GitHub Releases. The old local-candidate packaging script is not the Alpha promotion command.

See [GitHub release updates](github-release-updates.md) for signing-key and platform artifact requirements. No public release/tag, signing secret, TestFlight upload, or recurring review agent is created by setting up this local workflow.

## Implementation and verification contract

The primary E2E owner is `scripts/verify-macos-app-workflow.py`. It drives the real packaging/switching commands and signed native apps rather than a fake installer. No test-only production switches are required.

Before implementation, these are the credible failure cases to cover:

- An ordinary development refresh or opening Alpha replaces the installed Alpha snapshot. Record bundle hashes before and after a development refresh and channel switch.
- Channels share a bundle identity, IPC listener, persistent data root, or worktree root. Check signed plist identities and query the actual engines through their own ports/data directories.
- An Alpha snapshot claims a commit while packaging dirty or untracked source. Refuse installation/update before compiling if the source is not committed and clean.
- A daily bundle lacks the extension host or provider worker, has an invalid signature, or reports the wrong version. Verify all three native binaries and their hashes, signatures, version metadata, and native About panel.
- A build, signing step, occupied IPC port, or graceful quit fails after destroying the last working app. Stage first, validate before swapping, never force-quit, and retain the old bundle until the new process and IPC are verified.
- A repeated first-install command or a normal launch unexpectedly updates Alpha. First-install must reject an existing installation; normal launch must not compile or replace it.
- A version change updates third-party dependencies, leaves internal packages/iOS marketing versions inconsistent, or accepts a malformed/older version. Validate the real Cargo metadata/lockfile; exercise version commands in a disposable Git checkout and retain their outputs.
- Release automation publishes from a mismatched tag or silently edits an existing release. Validate versions before platform builds; reject a published tag before publishing assets. Preparing source does not push a release tag.

Verification retains a timestamped directory with command logs, JSON results, native identity/process/IPC evidence, and hashes. Developer and daily data are preserved. The first implementation resets the inherited app version to `0.0.1`; protocol, schema, and independently versioned package numbers are not reset with it.

```sh
python3 scripts/verify-version-workflow.py
# Requires the installed Alpha; refreshes Dev, then returns to Alpha.
python3 scripts/verify-macos-app-workflow.py
```

The version verifier removes its disposable Git fixture. The macOS verifier checks actual signed apps and real engine identities, refuses an accidental reinstall and direct/renamed-bundle self-update, and proves Alpha's bundle hashes survive the ordinary Dev refresh and both channel switches. It leaves Alpha running for the requested daily-use handoff. It does not send provider prompts or mutate existing conversations. Native About-panel readback supplements the machine-readable evidence.
