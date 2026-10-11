# RM-00 (M0): direct host boundary and physical native slice

Source before the change: `146c66b5` (11 October 2026). Live status belongs to
`pending_tasks.py`.
- The failure cases were written before any code: [failures.md](failures.md).
- The reviewed contract for RM-01 onwards is in [contract.md](contract.md):
  the rebased baseline, identity model, endpoint/scope/receipt map and version
  policy.

## Result

A physical iPhone 16 Pro (iOS 27.0) on the home Wi-Fi read the harmless
descriptor from this Mac over pinned TLS 1.3:
- It verified the host's identity, both by IP and by mDNS name.
- It refused a wrong pin, a different host and a same-key/different-target
  host, as well as malformed tickets, endpoints and descriptors.
- The Swift-owned `URLSession` transport fed every result into the owned Rust
  core through UniFFI, including a late completion after cancel and a
  completion after the core was released.

All 51 checks passed in the final run, with no test hook in production code
([report](evidence/physical/report.json)).

This is M0 only. It supports neither pairing nor authenticated reads, and no
task operation. M1–M4 remain open.

## What changed

- **`luna-proto::remote_control`**: `RemoteTarget` moved here and `luna-rpc`
  re-exports it, so the wire shape and import paths are unchanged. New types:
  - `RemoteDescriptor`;
  - `HostTicket`, the trust half of the future QR;
  - the `remote-direct-v1` protocol name, the paths and the pin format.
- **New `luna-remote` crate** (Unix only):
  - `HostIdentity`: a profile-owned P-256 key and target. The key is `0600` in
    a `0700` directory; damaged or partial identities are refused and never
    regenerated. No certificate is stored: one is derived from the key at
    every start, so renewal keeps the pin.
  - `host::bind`: one explicit interface. It refuses wildcard, loopback, port 0
    and any port already used on loopback, such as LUNA's IPC.
  - `host::serve`: TLS only, two harmless routes, no request bodies, bounded
    headers and connections, and an event log.
  - `probe` example: runs the host for QA.
- **New `luna-mobile` crate**: the UniFFI core (`luna_core`), version 0.32.2,
  the same as Zeron.
  - `DirectHostProbe` validates the ticket before any network activity.
  - It drives a foreign `HostTransport` and accepts only the current
    request's completion.
  - It checks the descriptor's protocol, catalog version, pin and target.
  - It reports states through a foreign `ProbeListener`.
- **`scripts/ios/build-mobile-core.sh`**: builds the iOS static library and
  Swift bindings through the locked Cargo wrapper.
- **Separate probe app `apps/ios/RemoteProbe`**
  (`com.novnski.luna.ios.remoteprobe`): `PinnedTransport.swift` holds the
  trust evaluation:
  - the challenge host must match;
  - the server key must be P-256 and its SubjectPublicKeyInfo SHA-256 must
    equal the pin;
  - the pinned leaf is the only anchor, evaluated with the basic X.509 policy.

  The shipping LUNA iOS app, its project and its CI are untouched. The probe
  builds in Swift 6 with complete strict concurrency and no warnings.
- **Rebase of the T3 audit:** its manifest is re-pinned from a rewritten SHA
  to the landed `7b269800`, and its verifier passes again.
- **CI:** `ui-tests.yml` checks that `luna-remote` and `luna-mobile` compile.

## Evidence (physical LAN)

Final run `rm00-20261011T185915Z`, Mac at `192.168.18.2`, phone at
`192.168.18.3` on Wi-Fi.

| Case | Result |
| --- | --- |
| H1 identity stable | Restart kept the pin and target; `openssl` recomputed the same pin |
| H2 damaged identities | 8/8 refused, no listener: world-readable key, group-readable directory, symlinked key, corrupt PEM, RSA key, invalid target, missing target, missing key |
| H3 bind rules | Refused `0.0.0.0`, `::`, loopback, port 0, a LAN port in use and a port held on loopback. With the refreshed LUNA Dev running, it also refused LUNA's real IPC port 59777, and LUNA Dev kept listening ([host run](evidence/host-with-luna-dev/report.json), 26/26) |
| H4 routes | Health and descriptor expose only their documented keys. Unknown paths, engine method names and RPC frames are refused (404/413); POST is 405; bodies are 413 |
| H5 plain HTTP | No HTTP response; the host logged a failed handshake |
| P1/P2 ticket and endpoint | 10/10 refused before any network activity; the sentinel host saw 0 connections |
| P3 wrong pin | `trustRejected`; the presented key was recorded; that host saw 1 handshake and 0 requests |
| P4 swapped host | `trustRejected`; host B saw 1 handshake and 0 requests |
| P5 same key, other target | `targetMismatch`, naming both targets |
| P6/P7 bad descriptors | Unsupported protocol and catalog, pin mismatch, oversized, malformed and HTTP 500 each refused with its own reason |
| P8 route | `192.168.18.3 → 192.168.18.2` over Wi-Fi, TLS 1.3, not proxied, not cellular. The host saw the same peer |
| P9 changed address | The mDNS name verified the same host: the pin, not the name, is the anchor |
| B1 bridge | The core drove the Swift transport; the Swift completion re-entered the core; listener callbacks arrived on main and background threads |
| B2 late reply | After `cancel()`, the cancelled request's completion was still delivered and the state stayed `cancelled` |
| B3 released core | The core was deallocated while the read was in flight; the later completion was dropped safely |

**Suites run:**
- `luna-proto` 35 passed, `luna-rpc` 13 passed, `device_room` 14 passed
  (1 ignored), `subscription_lifecycle` 7 passed.
- Engine `remote_auth` 5 passed and `device_routing` 5 passed: moving
  `RemoteTarget` left remote authentication unchanged.
- Clippy reports nothing in the new or changed code; rustfmt was applied.
- The probe app builds in Swift 6 with complete strict concurrency and no
  warnings.
- LUNA Dev was refreshed with this build: 188 s, PID 27919, IPC 59777,
  `codesign --verify --deep --strict` passes.

Evidence files:
- [run output](evidence/physical/run.out) and [plan](evidence/physical/plan.json);
- host checks with LUNA Dev running: [report](evidence/host-with-luna-dev/report.json) and [output](evidence/host-with-luna-dev/run.out);
- [phone results](evidence/physical/phone-result.json) and
  [phone console](evidence/physical/phone-console.log);
- one log per host: `evidence/physical/host-*.jsonl`;
- [audit rebase](evidence/audit-rebase.json) and the
  [T3 verifier result](evidence/t3-verify.json);
- commands and exit codes: [commands.json](evidence/commands.json);
- hashes: [sha256.json](evidence/sha256.json).

The retained runs are in `target/qa/control-this-mac/` (identities excluded
from the hashes).

**Earlier runs, kept honest:**

| Run | Checks passed | Cause |
| --- | --- | --- |
| `…185031Z` | 24/51 | iOS refused to launch an app from an untrusted Personal Team profile; the owner trusted it on the phone |
| `…185335Z` | 45/51 | The macOS firewall, in stealth mode, silently dropped LAN traffic to Homebrew's ad-hoc-signed Python, which hosts the scripted fixture. The runner now documents Apple's `/usr/bin/python3` |
| `…185632Z` | 51/51 | Passed, but the slow host and a reply counter were production test hooks. Both were removed: the slow host is now a scripted fixture, and B2 is proven by transport events plus final state |
| `…185915Z` | 51/51 | Final run, with no test hooks |

## Repeat

```sh
scripts/cargo.sh build --locked -p luna-remote --example probe
scripts/ios/build-mobile-core.sh iphoneos apps/ios/RemoteProbe/Generated
xcodebuild -project apps/ios/RemoteProbe.xcodeproj -scheme RemoteProbe \
  -destination id=<udid> -derivedDataPath target/ios-probe-dd \
  -allowProvisioningUpdates [DEVELOPMENT_TEAM=<team in your Xcode account>] build
/usr/bin/python3 scripts/qa-remote-direct-m0.py --device <udid>
```

- The phone must be unlocked on the same LAN.
- The first launch asks for local-network access; the app waits up to 150 s.
- A Personal Team build must also be trusted once under VPN & Device
  Management.

## Limitations and hand-off

- **Signing.** The probe was signed with the owner's free Personal Team
  (`JDC33NUDHX`), because Xcode has no account for the LUNA team
  `5XY3M483YQ`. The committed project keeps the LUNA team. Personal Team
  profiles expire after 7 days.
- **No simulator build.** `aarch64-apple-ios-sim` isn't installed.
- **Not exercised in M0**, by design: local-network denial recovery,
  certificate expiry and rotation, camera QR scanning, authenticated routes
  and any task data (see [failures.md](failures.md)).
- **Next owners.** RM-01 builds the engine-owned gateway on `luna-remote`.
  IOS-00 grows `luna-mobile` and moves the transport into the LUNA app.
