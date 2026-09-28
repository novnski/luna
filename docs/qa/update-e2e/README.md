# GitHub release updater E2E

`scripts/verify-updates-e2e.sh` creates a disposable, locally hosted release
with the same `manifest.json`, raw Ed25519 `manifest.sig`, and versioned app
tarball format as GitHub Releases. It exercises the built `luna update` command
and records a repeatable evidence directory under `target/update-e2e/`.

## Failure cases to exercise before accepting the updater

1. A modified manifest with its original signature, a modified signature, a
   missing signature, and a signed manifest without a valid SHA-256 must all
   fail before the app bundle is replaced.
2. A signed manifest with an artifact URL on another origin, an unsafe version
   or filename, or an older/equal version must not select an update.
3. A correctly signed manifest pointing at a payload with the wrong SHA-256,
   a truncated download, or an invalid app archive must leave the installed
   bundle and its data intact. No partial archive may be mistaken for a later
   successful download.
4. A downloaded app with a different bundle identity or invalid code signature
   must not replace the installed app. A failed copy, rename, or relaunch must
   preserve or recover the previous working bundle.
5. A valid signed update must replace only the chosen QA bundle, keep its
   isolated data and bundle identity, launch the new executable from the same
   path, listen on the assigned QA IPC port, and pass strict `codesign` check.
6. The test must refuse to operate on the primary LUNA bundle or data, and
   must not start a second Luna GUI app while another is running.

The scripted gate covers signature tampering, signed metadata validation,
payload checksum failure, an invalid archive, and the valid install/relaunch
path. Cases involving the graphical Updates button, blocked quit, filesystem
permission faults, interrupted network delivery, and a second actually
compiled app version require a native release trial.

## Run and evidence

Run from the repository root after the current Luna GUI app has saved and
quit gracefully. The harness reserves `LUNA QA Updates`, bundle ID
`com.novnski.luna.qa.updates`, IPC port `59788`, and data under
`target/luna-qa-data/updates/`. It uses a fresh local test signing key. It does
not use the production release private key or contact GitHub.
The harness re-signs only this disposable QA app after replacing its
development-only plist version with the numeric Cargo version, matching a
production release bundle.

```bash
DEVELOPER_DIR=/Library/Developer/CommandLineTools scripts/verify-updates-e2e.sh
```

The harness writes `result.json`, command logs, the exact manifest and
signature bytes, file checksums, old/new bundle identity, and process/port
readback to a timestamped directory under `target/update-e2e/`. Its
`SHA256SUMS` covers the retained files and can be checked with
`shasum -a 256 -c SHA256SUMS` from inside that directory. Keep that directory
as the verifiable artifact for a successful run. A failed run retains its
evidence for diagnosis. Review the recorded result before removing a
disposable QA bundle. Do not delete QA data or the shared Cargo cache during
this cleanup.

The payload is a signed clone of the current executable with a new plist
version and a marker file. This proves the download, trust check, bundle swap,
and relaunch mechanics. A two-release trial using two actually compiled
versions is still needed before publishing the feature. For graphical
acceptance, run that trial in an isolated QA app while the primary app is
closed: open **Updates**, check for the release, press **Download and restart**,
and record the old/new PID, version, bundle signature, and IPC port. Quit the
QA app gracefully before reopening `LUNA Dev`.
