# RM-02: Connections and native phone pairing

Live ownership and accepted criteria belong to the shared queue. The baseline
is `a31db907`, with RM-01's engine gateway, scoped authority and signed socket
tickets already delivered. This package records RM-02's bounded implementation
and [failure contract](failures.md) before runtime changes.

## Delivery plan

1. Extend the existing remote wire/mobile-core boundary for one validated
   enrollment operation. Reuse the pinned descriptor validation, engine pairing
   and session/ticket routes. Add no second engine, cloud service or reconnect
   runtime. Define QR/manual payload, expiry, uncertainty and persistence states.
2. Add Settings → Connections → Control this Mac with explicit interface/port,
   host readiness, Add/Cancel, refresh, authority roster, last authenticated use,
   revoke and a persisted remote-host keep-awake preference. Preserve Devices as
   registry metadata; remote trust never comes from registry presence. Reuse the
   existing power adapter/coordinator if applying the preference now; RM-07
   retains full availability/power and outside-network acceptance ownership.
3. Wire the existing LUNA iOS app to the owned mobile core, retaining its bundle,
   signing, current edge account and data. Add saved Macs, camera/manual entry,
   explicit confirmation, pinned HTTPS/WS, device-only scoped Keychain sessions,
   uncertain/re-pair states and local Forget. Incoming content cannot auto-pair.
4. Run the real engine/transport/restore boundaries, native UI and physical-phone
   pairing/restart/failure/forget-versus-revoke campaign. Retain sanitized evidence,
   commands, exit codes and hashes; refresh/verify LUNA Dev and deliver on main.

The pairing default is five-minute one-use bootstrap, thirty-day bounded device
session and `environment-read` plus `tasks-read` only. Terminal/file/task mutation
permissions retain their separately scoped owners. The phone must say paired or
connected only after actual persistence/authentication, respectively. A ready
listener is not proof that this phone reached it. No relay is introduced.

## Design intent

The user is the owner pairing an awake Mac with a known phone. The action is
establishing and reviewing trust; the feel is calm, compact native settings.
Use existing LUNA Geist typography, glass/card layering, spacing and foreground,
muted, accent, success and error tokens. Domain vocabulary is host, profile,
pairing, grant, route, session, expiry and revoke. The signature is three separate
truths: host ready, phone authenticated, route observed. Do not conflate the
registry with authority, treat a socket as authenticated, or show availability
from configured preferences. This extends the established design and authorized
scope; it does not require a new visual direction or font system.

## Testing ownership

The authoring gate is [failures.md](failures.md). Extend existing engine remote
owners for host semantics; put enrollment journeys at the real pinned native
transport/core/Keychain boundary. UI acceptance uses actual labeled controls and
readback. No new unit tests after runtime code, test-only production seams or
synthetic callbacks masquerading as OS/device proof. Simulator, scripted fault,
physical phone, camera and route evidence are distinguished explicitly.

The phone is currently discoverable with Developer Mode enabled. The saved WDA
helper's provisioning expired; reuse its private source/signing setup if control
is needed. Never enable Developer Mode, restart the phone or alter its VPN just
for QA. Pending external prerequisites do not block independent implementation.

## Minimal mobile enrollment facade

The shared core produces bounded transport actions, accepts exact request-ID
replies and owns preview/enrollment/persistence/authentication transitions.
Swift performs those requests through the existing pin evaluator and writes
only the returned session to its scoped Keychain. The main iOS app links the
same owned mobile library as M0; IOS-00 retains the broader public SDK/native
migration and RM-03 retains task projection/reconnect ownership.

A QR/manual code is `luna://pair#<base64url JSON>` carrying a versioned trust
host ticket, pairing ID, one-use credential, expiry and declared scopes. The
native fragment is opaque enrollment input and never opened in a browser or
sent as an HTTP URL. Manual entry may override an origin only after the same
pin/descriptor checks. Confirmation is required in the app before a credential
is spent. Diagnostics and captures omit the code, bearer and signed tickets.

## Owner clarification: standard, quiet credential storage

The owner asked to remove unnecessary features and specifically avoid password,
biometric or Keychain approval friction. Source comparison on 12 October:
- T3 `477282263a9115f911380b5aec7d4973267e4edc` uses ordinary Expo SecureStore
  get/set/delete calls with no authentication options in
  `apps/mobile/src/persistence/mobile-secure-storage.ts`.
- Zeron `40091d469aa8a3f1ab1ec7cb404957b742940278` uses plain SecItemAdd/read
  with AfterFirstUnlock accessibility in `apps/ios/Zeron/App/AppModel.swift`.
- LUNA already uses the same plain APIs in `Luna/Auth/AuthClient.swift`.

Follow this pattern for remembering the pairing token, using a scoped account
and device-only accessibility. Reuse the existing iOS credential helper rather
than introducing a vault, custom encryption system, password/Face ID gate,
credential-manager UI or Mac Keychain reads. Store only the token as a secret;
route/name/pin/session metadata uses ordinary local storage. Handle actual OS
status so a failed write cannot be reported as saved. A locked phone can require
ordinary unlocking; pairing itself adds no credential-entry approval prompt.

Expo's native implementation adds biometric access control only for the
explicit requireAuthentication option:
https://github.com/expo/expo/blob/main/packages/expo-secure-store/ios/SecureStoreModule.swift

## Implemented behavior

- Connections reads the active engine's readiness and issued grants. Add creates
  a five-minute QR/manual fragment, Cancel revokes it, disabling hosting removes
  pending codes, and Revoke affects the selected session. Engine and sheet
  generations fence late results. Keep awake uses the existing coordinator,
  applies only on external power, and reports actual assertion success/failure.
- The iOS sign-in screen and account menu open Paired Macs. Camera and manual
  entry share validation and explicit confirmation. The native transport checks
  the expected certificate key and the shared core checks the exact host/profile
  descriptor before sending a credential. Successful secure storage precedes
  authenticated session readback. Foreground/connect revalidates saved access.
- Bearers use the existing app-owned iOS secure-store helper, in a separate
  paired-Macs service with a host/profile scoped account. Device-only items do
  not synchronize or require user presence. Reads explicitly disallow UI.
  Preferences contain route/identity/grant metadata. No password or biometric
  authentication flow was added. Forget removes only the selected local record;
  host revoke requires explicit re-pairing on the next authentication.
- Connection status describes the last successful authenticated HTTP check.
  Continuous RPC streams and network reconnect ownership remain RM-03. Pairing
  does not yet add the later remote task-control interface.

## Verification and repeatable native run

The user explicitly chose Simulator verification and released the physical phone
on 2026-10-12. Physical QR capture and preview were observed. The physical LAN
connection was refused/unreachable with the QA Mac bundle's firewall state;
physical pairing/restart is not certified by the Simulator run. No firewall rule
was changed. The phone helper was stopped with process/listener readback.

The native E2E uses two real engine gateways, TLS pinning, the shipped SwiftUI
controls, actual iOS Simulator secure storage, app relaunch and separate host
revoke. It also verifies camera refusal/unavailability and local Forget retaining
the other Mac's access. A compile-only Simulator build without app entitlements
produced a real OS storage refusal; the app reported re-pair and never Connected.
Normal Simulator signing generated its app identifier, and the native run passed
with zero skips. Controlled lost/oversized reply, failed persistence and cancelled
save callbacks are separately exercised after real host redemption; they are not
represented as physical network faults or physical Keychain failures.

Repeat on an owned disposable Simulator:

1. Use XcodeBuildMCP session defaults for `apps/ios/Luna.xcodeproj`, scheme Luna,
   the Simulator and a task-scoped DerivedData directory. Build prepared test
   products with `build_sim({buildForTesting:true})` and normal signing; install
   the resulting `LunaUITests-Runner.app` once to create its private container.
2. Run `python3 scripts/qa-native-pairing-sim.py --simulator <udid> --run-dir
   <new-target-qa-directory>`. It starts the real gateway campaign and reports
   fixture readiness without printing bootstrap or bearer values. It follows
   the runner container when Xcode reinstalls it.
3. Use XcodeBuildMCP `test_sim` with those prepared products, `progress:false`,
   and `extraArgs:["-parallel-testing-enabled","NO",
   "-only-testing:LunaUITests/RemotePairingUITests/testManualPairingRestartAndLocalForget"]`.
   Prepared products avoid a nested Cargo build while the host owner is running.
4. Require both the native test and host campaign to exit zero, inspect the
   public `summary.json` and retained `.xcresult`, then hash the evidence. The
   coordinator removes its private bootstrap files. Raw XCTest logs remain
   private because XCTest records typed text. Publish sanitized summaries only.

## Delivered checks and handoff

- Final native XCTest: **1 passed, 0 failed, 0 skipped**, 114.8 seconds. The
  real host campaign also exited zero, including wrong native pin/profile
  refusal, secure save/relaunch, other-host preservation and host-revoke denial.
- Engine remote-access suite: **6 passed**. The pending-code disable regression
  demonstrably failed before the repair and passed afterward.
- RPC published-contract check: **3 passed** in its isolated CI-equivalent
  invocation. Combining it with the engine suite unified serde features and
  changed JSON property ordering; no schema content change was needed.
- Changed Rust files passed rustfmt and Clippy; the iOS app/test products built
  with normal Simulator signing and no diagnostics. Retained evidence includes
  sanitized commands/results, public native observations and SHA-256 hashes.
- Feature `ef5cfe97` merged as `16be9f7b`; `origin/main` was verified. Primary
  LUNA Dev refreshed in **107 seconds**, exact PID **19546**, IPC **59777** and
  strict signature verified. The delivered Connections controls were read back
  in the refreshed primary app. The stopped QA app alone was unregistered and
  removed; its data and build caches remain. Physical phone control is stopped.
