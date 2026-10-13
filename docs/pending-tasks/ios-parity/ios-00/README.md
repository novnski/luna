# IOS-00: owned iOS SDK facade and migration build

Source before the change: `8758745a` (13 October 2026). Live status belongs to
`pending_tasks.py`. This is the contract and the evidence plan; the
orchestrator records results here once it has run them. The failure cases
below were written before the code.

## Baseline (rebased on current code)

The parity research said the workspace had "no owned Rust mobile facade".
That is out of date: RM-00 and RM-02 added one. IOS-00 grows it.

- **RM-00:** `crates/mobile` (`luna-mobile`, UniFFI 0.32.2, namespace
  `luna_core`) with `DirectHostProbe`; Swift owns `URLSession` and the pin
  (`SharedRemote/PinnedTrust.swift`); `scripts/ios/build-mobile-core.sh`.
- **RM-02:** `Enrollment` in the core; the shipping app links the core
  through the "Build owned mobile core" phase, commits
  `Luna/Generated/luna_core.swift`, and stores paired Macs as
  `luna.paired-macs.v1` (preferences, no secret) plus one device-only token
  per host in the `com.novnski.luna.ios.paired-macs` Keychain service.
- **RM-01:** the gateway serves `descriptor`, `pair`, `session/check`,
  `socket-ticket` and `ws`; each socket runs the per-session remote guard, at
  most 32 requests or streams in flight, 1 MiB messages.
- **CORE-05:** `luna-sdk` 0.1.0, API 1, over `luna_rpc::RpcClient`.
  `DescribeApi` stays owner-only; RM-03 decides remote discovery.

**Gaps found:**
1. The core did not link `luna-sdk`: no typed operation reached Swift, and the
   phone had no socket path at all.
2. The build always used Cargo's dev profile and one architecture. A Release
   archive linked an unoptimized core, a multi-architecture simulator build
   could not link, and nothing recorded what was built.
3. Nothing checked that the committed Swift bindings match the core.
4. The descriptor's catalog version was checked against a literal `[1]`, not
   the SDK's API.
5. UniFFI's Swift async never cancels the Rust future when the Swift task is
   cancelled (`uniffiRustCallAsync` waits for completion): there was no
   cancellation path.
6. The running library had no identity a build or support record could cite.

## What changed

- **`luna_mobile::session::RemoteSession`** (facade v1). It is built from
  saved access, validated with the same rules as RM-02's restore. `connect`
  takes four steps:
  1. a pinned descriptor read: host, pin, `remote-direct-v1`, and a catalog
     equal to `luna_sdk::API_VERSION`;
  2. a single-use socket ticket bought with the saved bearer;
  3. a pinned WebSocket that the platform opens with that ticket;
  4. `luna_sdk::Client::engine_info` over the socket's frames, which must name
     the same device and workspace scope and advertise the required
     capabilities.

  After that, `engine_info` and `watch_task_commands` run through the same
  SDK client. Frames go through `luna_rpc::RpcClient::new` over two bounded
  channels.
- **`luna_mobile::runtime`:** one process-wide two-worker Tokio runtime, and
  `Cancellation`, which the platform cancels. Every exported async call runs
  on that runtime, aborts when its awaiting future is dropped, and returns
  `cancelled` once its `Cancellation` fires.
- **`core_identity()` and `FACADE_VERSION`:** facade, core and SDK versions,
  API version, UniFFI contract version, target and debug or release build.
- **The probe, enrollment and session** now share
  `SUPPORTED_CATALOG_VERSIONS = [luna_sdk::API_VERSION]`, and restore and
  session share one `validate_saved`.
- **Swift:**
  - `Remote/PinnedSocketTransport.swift`: the pinned HTTPS requests and one
    pinned `URLSessionWebSocketTask`, reported back to the core. It sends
    pings to keep within `URLSession`'s idle timeout and never reopens.
  - `Remote/RemoteHostSession.swift`: a `@MainActor` owner. It bridges
    Swift task cancellation into `Cancellation`, re-reads state on the main
    actor after each notification, and wraps watches.
  - `PairedMacsModel` opens one live session after RM-02's authenticated
    connect, and closes it on disconnect, Forget, Done and background.
  - The Paired Macs card shows `Live · host <id8> · API 1`, and the footer
    shows the linked core.
- **Build (`scripts/ios/build-mobile-core.sh`):**
  - `CONFIGURATION` picks the profile: Debug uses Cargo's dev profile,
    Release uses `--release`, and anything else is refused.
  - `ARCHS` picks the Rust targets (arm64 device, arm64 or x86_64
    simulator), joined with `lipo`.
  - An `IPHONEOS_DEPLOYMENT_TARGET` other than 26.0 is refused.
  - Bindings are generated from the first library actually linked.
  - `target/ios-core/<platform>/identity.json` records what was built. The
    build stays bash 3.2 safe.
- **CI** (`ui-tests.yml`, iOS job): after the simulator build has
  regenerated the bindings, `git diff --exit-code` fails if the committed
  copy differs.

## Contract

**SDK and ABI.**
- The facade calls only `luna_sdk::Client`; `luna-mobile` contains no wire
  method string. Its typed operations are `connect` (descriptor plus
  `engineInfo`), `engineInfo` and `watchTaskCommands`. Adding one is an SDK
  operation first, then a facade method.
- Version gate. The descriptor must say `remote-direct-v1` and name the SDK's
  catalog; otherwise the session is `incompatible` and no credential is used.
  `DescribeApi` is not called remotely (it is local-only).
- Binding identity: UniFFI's contract version and per-call checksums stop the
  app at its first core call if the bindings and the library differ. The
  build regenerates the bindings from the linked library on every build, CI
  checks the committed copy, `identity.json` hashes both, and
  `core_identity()` reports the running library.
- `FACADE_VERSION` (1) is bumped for any Swift-incompatible change to an
  exported type or call.

**Actors and threads.**
- Core calls are safe from any thread, and no lock is held while foreign code
  runs.
- Transport callbacks may arrive late, on any thread, or synchronously.
  Replies for anything but the current request or socket are ignored.
- The listener only says that the state changed. `RemoteHostSession` hops to
  the main actor and re-reads `state()`, so notifications arriving out of
  order cannot reorder states. Every awaited result resumes on the caller's
  actor (the main actor in the app).

**Cancellation.**
- **Read:** a cancelled `engineInfo` drops its pending reply slot. A late
  reply is discarded, and the session stays open.
- **Watch:** `cancel()`, releasing the handle, or cancelling the Swift task
  awaiting `next()` aborts the forwarding task. That drops the SDK watch,
  whose subscription sends `{id, cancel: true}`, and the engine releases the
  stream and its slot.
- **Connect:** a cancelled connect closes the session (`cancelled`), so a
  half-open socket never outlives it.
- **Session:** `close()` or releasing the session ends pending calls with
  `closed`, aborts every watch and the outbound pump, and tells the
  transport to close.

**Connection lifetime.**
- One session is one socket, and it never reconnects. A closed session
  (`local`, `cancelled`, `remote`, `rePair`, `rejected`, `failed`) stays
  closed; the caller creates a new one. This is the single connection owner
  RM-03 extends with reconnect, caches and projections; RM-03 must not add a
  second one.
- RM-02's HTTP session check still runs first on connect, unchanged, so its
  certified pairing UI does not regress. Once RM-03 owns reconnect, it can
  replace that check with the ticket's own bearer check.

**Old-edge and direct-profile data.** IOS-00 writes no new persisted key and
rewrites no existing record:

| Data | Store and key | IOS-00 |
| --- | --- | --- |
| Edge account tokens | Keychain `com.novnski.luna.ios` (`accessToken`, `refreshToken`) | Untouched |
| Edge settings | Preferences `edgeURL`, `authMode`, `userId`, `orgId`, `deviceId`, `homeSpaceFilter`, `newSession*` | Untouched |
| Edge document cache | `Application Support/LunaDocs/*.loro`, registry blobs | Untouched |
| Paired-Mac metadata | Preferences `luna.paired-macs.v1` (Codable JSON, no secret) | Read with the unchanged decoder; RM-02 still updates `lastAuthenticated`, `route` and `needsPairing` on connect |
| Paired-Mac tokens | Keychain `com.novnski.luna.ios.paired-macs`, account SHA-256(device, environment, scope) | Read only; Forget and re-pair behave as in RM-02 |
| Live session | Memory only | New; never persisted |

- **Upgrade** from any RM-02 build therefore keeps every byte.
- **Downgrade** to an RM-02 build reads the same data, because no field or
  key was added.
- **Future rule:** a new direct-profile shape goes under a new key (for
  example `luna.paired-macs.v2`) written alongside v1. v1 stays readable for
  older builds until a release decision retires it; it is never reinterpreted
  in place. Edge data is never read as direct data, or the reverse, and the
  facade never deletes edge data.

**Bundle, signing and floor.** `com.novnski.luna.ios`, its test bundle IDs,
team `5XY3M483YQ`, automatic signing and `IPHONEOS_DEPLOYMENT_TARGET = 26.0`
are unchanged. The project file is not edited: the new Swift files join
through the synchronized folders. The core build refuses another floor.

**License and provenance.**
- **No upstream source copied.** The runtime technique (a process runtime,
  with each async export spawned and awaited so the futures run on any
  executor) matches Zeron's `crates/client/src/runtime.rs` at `40091d46`
  (MIT, © 2026 Wing, the same notice as LUNA's own `LICENSE`). LUNA's code
  was written independently and adds abort-on-drop and explicit
  cancellation. No Zeron bundle ID, App Group, endpoint, signing asset or
  service is used.
- **UniFFI.** uniffi-rs 0.32.2 is MPL-2.0. The generated
  `luna_core.swift` stays in its own file; the only edit is the existing
  trailing-whitespace normalization. Notices for store distribution are an
  IOS-14 review item.
- **Linked dependencies.** Linking `luna-sdk` brings `luna-rpc`'s dependency
  graph into the iOS static library (Tokio, tokio-tungstenite with rustls,
  `luna-sync` with loro and bundled SQLite), although the phone uses only the
  frame multiplexer. Dead-stripping drops unreferenced code. The licenses of
  what is actually linked must be inventoried before any distribution
  (IOS-14). A later change can feature-gate `luna-rpc`'s device room and
  dialer (proposal; it is outside IOS-00's code areas).

## Failure contract (before code) and owners

The E2E owners are:
- **Rust E2E:** `remote_access::gateway::mobile_session_reads_typed_identity_and_every_cancellation_reaches_the_engine`
  (real gateway, Rust stand-in transport, runs in CI).
- **Native E2E:** `LunaTests/RemoteSessionTests` (the shipped Swift transport,
  in Simulator, against the same host).
- **UI:** the existing `LunaUITests/RemotePairingUITests`, extended.

| # | Failure | Owner and proof |
| --- | --- | --- |
| F1 | The bindings and the library differ | UniFFI checksum and contract check at first call. The build phase regenerates from the linked library, CI fails on drift, and the hashes in `identity.json` match the committed file |
| F2 | Wrong architecture or profile: simulator core in a device build, missing x86_64, Release linking a dev core | Build script plus `identity.json`, `lipo -archs` and the Xcode builds below (simulator Debug, device Debug, device Release, generic multi-arch simulator) |
| F3 | The deployment floor drifts | The build script refuses it; `-showBuildSettings` readback |
| F4 | Bundle or signing identity changed | No `project.pbxproj` change; `-showBuildSettings` and `codesign -dv` readback |
| F5 | A second client or runtime | Review: no method string in `crates/mobile`, every call through `luna_sdk::Client` |
| F6 | The wrong host answers | Rust E2E: a contradicting profile is `identityMismatch` with only the descriptor requested (the bearer is never offered). Rust and native E2E: `engineInfo` names the engine's device. UI: the live line shows the paired host's ID |
| F7 | Another catalog is driven anyway | The shared `verify_descriptor`, whose refusal cases RM-00 P6 exercised. No host serves catalog 2 (limitation) |
| F8 | A cancellation is lost | Rust and native E2E: 40 open-and-cancel cycles on one socket under the gateway's 32-slot bound, each ending within 5 s. Native: a cancelled Swift task ends a pending `next()` |
| F9 | A late reply changes state | Rust E2E: a cancelled `engineInfo` returns `cancelled`, and the same socket then reads (one socket opened in total). Native: the same socket reads after 41 cancellations |
| F10 | A callback lands on the wrong actor or thread | Native E2E: results resume on main, and each `onChange` (including the remote close) runs on main |
| F11 | A socket or watch is orphaned after release | Rust E2E: dropping the session ends its watch and closes the transport. Native: releasing `RemoteHostSession` ends its watch |
| F12 | Revoked or expired access still works | Rust and native E2E: a revoke closes the open socket (`remote`), and the next connect is `rePair` |
| F13 | A destructive migration | No new keys (the table above). Native QA upgrade and downgrade with `qa-data-continuity-sim.py`, plus Connect readback |
| F14 | Copied upstream identity or service | Review and grep: no Zeron identifiers |
| F15 | A secret leaks into a URL or log | Rust E2E: no request URL contains the bearer. The ticket travels only in `Authorization`. The coordinator fails on any secret-named preference key. Neither the core nor the transport logs |
| F16 | Unbounded frames or memory | 1 MiB message cap on both sides, bounded frame and watch queues, an awaited `receive` that applies back-pressure (contract) |
| F17 | A double connect | Rust E2E: a second `connect` is `busy` and leaves the session working |

## Criteria mapping

- **contract:** this document, `crates/mobile/src/{lib,runtime,session}.rs`,
  `Remote/RemoteHostSession.swift`.
- **delivery:**
  - the session facade on `luna-sdk`, and `PinnedSocketTransport`;
  - the app wiring and live line;
  - the configuration- and arch-aware build with `identity.json`, and the CI
    drift step.

  The typed read is the descriptor plus `engineInfo`; the cancellation is the
  watch stream and the read. No test-only API exists.
- **verification:** Rust E2E, native E2E, the UI extension, the build
  identity readbacks and the migration QA below, plus the license and
  provenance decisions above.
- **handoff:** the orchestrator's builds, E2E, lint, review, evidence and
  delivery. `luna-mobile` is not linked by the Mac app, so no LUNA Dev
  refresh is required for this change (only an engine test file changed on
  the Mac side).

## Commands (orchestrator)

**1. Lockfile and Rust.** `luna-mobile` gains `luna-rpc`, `luna-sdk`,
`serde` and `tokio`; no new external crate. Update `Cargo.lock` first, then:

```sh
scripts/cargo.sh check --locked -p luna-mobile --all-targets
scripts/cargo.sh check --locked -p luna-mobile --features bindgen --bin uniffi-bindgen
scripts/cargo.sh clippy --locked -p luna-mobile --all-targets
scripts/cargo.sh check --locked -p luna-engine --tests
CARGO_INCREMENTAL=0 LUNA_REMOTE_TEST_IP=<this Mac's Wi-Fi IP> scripts/cargo.sh test --locked \
  -p luna-engine --test remote_access gateway:: -- --test-threads=1
```

- **Expected:** all `gateway::` tests pass, including the new
  `mobile_session_…` and RM-02's enrollment test, which ignores the new
  `deviceId` fixture field unless its native mode is used.
- **Without `LUNA_REMOTE_TEST_IP` on this Mac,** the gateway tests print
  SKIPPED (default VPN route); Linux CI runs them on the LAN route.

**2. iOS builds and identity (no device signing needed).** Run from
`apps/ios`, with `DD=../../target/ios-dd/ios00`:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
xcodebuild -project Luna.xcodeproj -scheme Luna -configuration Debug \
  -destination "platform=iOS Simulator,id=$SIM" -derivedDataPath $DD build-for-testing
cat ../../target/ios-core/iphonesimulator/identity.json
xcodebuild -project Luna.xcodeproj -scheme Luna -configuration Debug \
  -destination generic/platform=iOS -derivedDataPath $DD CODE_SIGNING_ALLOWED=NO build
xcodebuild -project Luna.xcodeproj -scheme Luna -configuration Release \
  -destination generic/platform=iOS -derivedDataPath $DD CODE_SIGNING_ALLOWED=NO build
cat ../../target/ios-core/iphoneos/identity.json      # cargoProfile release, rustTargets [aarch64-apple-ios]
xcodebuild -project Luna.xcodeproj -scheme Luna -configuration Release \
  -destination 'generic/platform=iOS Simulator' -derivedDataPath $DD CODE_SIGNING_ALLOWED=NO build
xcrun lipo -archs ../../target/ios-core/iphonesimulator/libluna_mobile.a   # arm64 x86_64
shasum -a 256 Luna/Generated/luna_core.swift          # equals identity.json bindings.swift
git diff --exit-code -- Luna/Generated/luna_core.swift Luna.xcodeproj
xcodebuild -project Luna.xcodeproj -scheme Luna -configuration Release -showBuildSettings \
  | grep -E '^ *(PRODUCT_BUNDLE_IDENTIFIER|DEVELOPMENT_TEAM|CODE_SIGN_STYLE|IPHONEOS_DEPLOYMENT_TARGET) ='
```

- **The bindings change with this core.** The first build rewrites
  `Luna/Generated/luna_core.swift`; commit that regenerated file, then the
  `git diff` check is clean.
- **Keep RemoteProbe's copy identical** by regenerating it from `apps/ios`
  with `../../scripts/ios/build-mobile-core.sh iphonesimulator RemoteProbe/Generated`
  (the probe project has no build phase). The output directory is relative
  to the directory you run it from, so from the repository root the
  equivalent is `scripts/ios/build-mobile-core.sh iphonesimulator apps/ios/RemoteProbe/Generated`.
- **Expected settings:** `com.novnski.luna.ios`, `5XY3M483YQ`, `Automatic`
  and `26.0`, unchanged.
- **Unit tests in CI:** run `-only-testing:LunaTests` as CI does.
  `RemoteSessionTests` skips there because it has no fixture.

**3. Native E2E (Simulator, real gateway).** Use an owned, disposable
Simulator and the products prepared in step 2.

```sh
xcrun simctl install $SIM $DD/Build/Products/Debug-iphonesimulator/Luna.app   # creates the container
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test remote_access --no-run
python3 scripts/ios/qa-native-session-sim.py --simulator $SIM \
  --run-dir target/qa/ios-00/<UTC stamp>/session --host-ip <Wi-Fi IP> &
# after it prints "Native fixture ready":
xcodebuild test-without-building -project apps/ios/Luna.xcodeproj -scheme Luna \
  -destination "platform=iOS Simulator,id=$SIM" -derivedDataPath target/ios-dd/ios00 \
  -parallel-testing-enabled NO -only-testing:LunaTests/RemoteSessionTests \
  -resultBundlePath target/qa/ios-00/<UTC stamp>/session/RemoteSessionTests.xcresult
```

Expected:
- 1 test passed, 0 skipped, and the coordinator exits 0.
- In `summary.json`, every `nativeObservations` flag is true, with
  `watchCycles` 40, `deviceId` equal to the host engine's, the `core`
  identity, and `route` (interface and TLS version). `secretKeysInPreferences`
  is empty.
- Publish only `summary.json` and hashes; the private host log may contain
  fixture paths.

**4. UI journey (RM-02's campaign, now with the live read).** Follow RM-02's
repeatable run (`docs/pending-tasks/control-this-mac/rm-02/README.md`, steps
1–4; `scripts/qa-native-pairing-sim.py` pins `LUNA_REMOTE_TEST_IP` to this
Mac). Expected: `testManualPairingRestartAndLocalForget` passes, and it now
also requires `Live · host <first 8 of the first host's device id>` on the
first card.

**5. Migration and downgrade.** Use a disposable Simulator and an isolated QA
Mac host: a QA LUNA app with all four `LUNA_DEV_*` overrides, or a
disposable engine; never the primary profile.
1. Build the baseline app from `8758745a` (for example a detached QA worktree
   with its own DerivedData) and install it. Launch it once, then pair one Mac
   through Paired Macs → Add Mac → manual code; the "Connected" state shows.
   Terminate the app (`xcrun simctl terminate $SIM com.novnski.luna.ios`).
2. `python3 scripts/ios/qa-data-continuity-sim.py snapshot --simulator $SIM --out <run>/baseline.json`
3. Install the IOS-00 build over it (same bundle ID). Launch it, open Paired
   Macs **without** connecting, then terminate it. Run
   `snapshot --out <run>/upgraded.json`, then
   `compare <run>/baseline.json <run>/upgraded.json`. The expected result is
   no unexpected change.
4. Launch, Connect: both "Connected" and the `Live · host …` line appear,
   which proves the Keychain token survived. Terminate, take
   `snapshot --out <run>/connected.json`, then
   `compare <run>/upgraded.json <run>/connected.json --allow 'paired-macs:*:lastAuthenticated' --allow 'paired-macs:*:route' --allow 'defaults:luna.paired-macs.v1'`.
5. Downgrade: install the baseline build over it, launch, Connect. Expect
   "Connected" (no live line in that build). Take `snapshot`, then compare
   against `connected.json` with the same allowances.
6. Afterwards, revoke the QA grant on the QA host, quit the QA host
   gracefully, and erase only the disposable Simulator.

## Limitations

- **No physical device run is specified here.** The device build is compiled
  (step 2) but not run on a phone. The physical-route and cellular claims
  stay with RM-07 and IOS-09.
- **No host serves catalog 2**, so version skew (F7) is proven only through
  the shared descriptor check.
- **An unknown task ID is watched.** The watched ID has no commands, so the
  engine answers its zero snapshot from the durable store. The stream, its
  cancellation and the gateway slot are real; task projections are RM-03's.
- **The in-flight unary cancellation is not observed on the host.** Cancelling
  a unary read is a client-side release by design: CORE-01 never cancels a
  server call. The E2Es prove the cancelled-read contract and socket reuse,
  not a host-side abort.
- **Liveness is ping-based.** The keepalive ping keeps the socket inside
  `URLSession`'s idle timeout and detects a silent Mac. A dropped socket is
  terminal; RM-03 owns reconnect.
- **The graph needs a license inventory.** The dependency graph linked into
  the iOS library needs that inventory before any distribution (IOS-14).
