# LUNA local adoption candidate

This procedure covers a **local, ad hoc macOS candidate**. It does not install
over another app, configure an update feed, import live data, or establish
public distribution readiness. The parent task owns native QA and acceptance.

## Evidence and limits

| Area | Accepted or available locally | Still unsupported or unverified |
| --- | --- | --- |
| Core L01–L28 | Parent-reviewed local implementation evidence in `luna-progress.md` and its linked acceptance records. | Deferred X features and unexercised real-provider variants are not included in the core count. |
| L17 backup and recovery | Private, unencrypted offline LUNA archive; SHA-256 verification, staging, allowlisted same-root restore, rollback directory and interrupted-restore recovery. | Cross-root/device restore, provider credentials, remote grants, arbitrary workspace data, and a general Zeron import. See [backup boundary](luna-backups.md). |
| L29–L31 remote | Local synthetic pairing/protocol, SSH ownership, and owner-relay tests accepted by the parent. Protocol v1 and `remote-auth-v1` are required; the L30 artifact is versioned and checksummed. | Real second host, VPS, external TLS, provider accounts and public relay deployment were not tested. An old or incompatible engine must be disconnected or upgraded, not treated as equivalent. |
| L32 package | `scripts/package-luna-local.sh` builds a versioned `LUNA.app` and ZIP under `target/package/l32-local/`, signs ad hoc and writes a SHA-256 manifest. The bundle identifier is `com.novnski.luna`; default data is `~/.luna`, owner IPC is `59778`, and updates remain disabled. | Developer ID signing, notarization, public feed/channels, upload and installation are outside this candidate. |
| Daily use | The packaged Mach-O has an explicit `native-fixture` path with mock-only registration, in-memory auth, disabled native metadata/Keychain fallback, no Edge link, and no local IPC listener. `--check-only` verifies it without a window. | Parent Computer readback is still required. A synthetic session is not sustained daily use with a real provider or live data. |

The package manifest binds upstream base version `0.2.73` (or the current
workspace version) to the exact LUNA Git revision and binary/ZIP hashes.
`Info.plist` carries the same base and revision. A clean tracked checkout is
required before packaging. The script never reads signing/notarization
credentials and refuses to overwrite a candidate at the same revision.

## Identity and compatibility checks

1. On an isolated test root, run the packaged binary's `status` and `inspect`
   commands with `LUNA_DATA_DIR` and `LUNA_IPC_PORT` set to that root and a
   test port. Bare startup must report local-only mode and no configured edge.
   Legacy `ZERON_EDGE_URL`, `ZERON_WORKOS_CLIENT_ID` and `ZERON_EDGE_TOKEN`
   must not opt LUNA into Zeron's service. An owned service requires explicit
   `LUNA_EDGE_URL` and, for WorkOS, `LUNA_WORKOS_CLIENT_ID`.
2. Confirm bundle ID, plist revision, ad hoc signature, binary hash and ZIP
   hash against `manifest.json`. A different product or a LUNA engine for a
   different canonical data root must be rejected before attachment, even if
   the two roots contain the same copied device ID. The path-free root claim
   is served only on local-owner IPC, never guarded remote RPC.
3. `luna update` remains an explicit refusal until an owned signed feed exists.
   L29 guarded RPC and the L30 headless artifact use their existing protocol,
   capability, exact-target and checksum gates. A stale engine or a capability
   mismatch is an error, not an automatic migration.

## Synthetic data rehearsal

Only an L17 archive from a **stopped synthetic LUNA root**, restored to the
same root with the same device identity, is supported by the existing offline
restore path. The archive includes supported settings, profile databases,
journals and committed attachments. It excludes credentials, provider homes,
active remote grants, worktrees, logs and derived caches. A preview must show
entry/database/attachment counts, bytes, destination, conflicts and unsupported
reasons before restore. Staging verifies hashes, supported schema and counts,
rebuilds supported derived indexes, reconciles referenced blobs, then reopens
the stage. The offline restore keeps a private rollback root; reopen readback
and `luna backup recover` exercise the failure boundary. Do not delete the
rollback root before readback.
Schema 12 backup claims include cleanup checkpoints and local executable
authority rows, while active remote grants remain excluded. Schema 9 archives
remain readable through staged migration; newer unreviewed schemas fail closed.

With `LUNA_DATA_DIR` set to that stopped synthetic root, run
`luna backup preview /absolute/path/to/archive.tar.gz` before any restore. It
reports unsupported reasons without activating anything. A copied root must
show the original-root limitation even when its device ID matches.

`local_import.rs` imports LUNA local-profile records into a synced LUNA profile;
it is **not** a Zeron-to-LUNA converter. A copied Zeron store or unknown archive
has no proven cross-product schema mapping here and must be reported as
unsupported. Never point this rehearsal at installed Zeron, the real LUNA
profile, `CODEX_HOME`, provider auth homes, Keychain, or a live workspace.

## Parent native QA setup

The worker does not launch the desktop. The parent chooses the newly built
candidate directory (`candidate` below) and prepares an empty private fixture
under the current system temp directory:

```sh
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/luna-l32-native.XXXXXX")
chmod 700 "$fixture"
printf 'L32 synthetic native fixture\n' > "$fixture/.luna-l32-native-fixture"
"$candidate/LUNA.app/Contents/MacOS/luna" native-fixture --root "$fixture" --check-only
```

The check-only command must report `local mock, Edge disabled` and exit 0.
It uses the **same packaged Mach-O** as the window. The fixture validates the
real, private, symlink-free root. A new root must contain only that marker;
after preparation, a second fixture marker allows safe reopen. All data and synthetic provider paths stay
under it. It opens only the in-memory credential broker and mock harness,
disables native Codex metadata and Claude Keychain fallback, and does not set
`HOME` or `CODEX_HOME` globally. The fixture uses in-memory RPC and port 0;
no daemon, remote listener, appshot service, or Edge supervisor starts.

The parent may launch the app through LaunchServices with the explicit mode:

```sh
open -n -a "$candidate/LUNA.app" --args native-fixture --root "$fixture"
```

The app's own entrypoint is the signed Rust executable, with no shell/exec
wrapper inside the bundle. Use supported Computer readback for first render,
local/mock identity, task/project navigation, settings, backup status, native
close/reopen and exact data-root persistence. Read only bounded excerpts from
`$fixture/logs/luna-native-fixture.log`. Quit the exact fixture app, verify its
process has exited, then repeat `--check-only` on the same root to confirm the
lock was released. Retain only chosen evidence. The L30 shell-wrapper path
rendered but timed out in AX, so a wrapper is not a substitute for this check.
Do not treat this synthetic smoke as sustained real-provider daily use.
