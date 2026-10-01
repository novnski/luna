# Validate direct remote control end to end

These are required future scenarios, not checks passed during planning. Write and exercise the relevant failure cases before changing the subsystem. Prefer E2E and integration coverage that verifies a real user journey and retains repeatable evidence. Do not add unit tests after implementation or create tests that only mirror framework wiring.

## Evidence contract

Each milestone owns an artifact directory under `target/qa/control-this-mac/<run-id>/` with a sanitized manifest, source revisions, exact host/mobile identities, route mode, build/check exit codes, timings, command receipts, event/cursor observations, bounded logs, and screenshots or recordings. Hash the retained evidence and document one repeatable command/procedure for the run.

Keep the raw QR credential, session bearer, socket ticket, provider secret, DNS credential, and sensitive workspace content out of published logs, screenshots, manifests, Git and clipboard history. A recording of Add must mask the machine-readable QR. Do not weaken the live QR in the product to make screenshots safe.

Record which evidence came from a deterministic fixture, simulator, physical phone, actual second network, or real provider. A fake network route and a seeded authority record cannot prove Internet reachability or real pairing.

## First failure inventory

### Listener and identity

- Allow connections is off on first launch; no direct listener or Bonjour advertisement is published.
- The remote port is occupied, equals local IPC, or belongs to another profile/process; startup fails without stealing it.
- TLS key/certificate or remote authority persistence is missing, corrupt, unsafe, unreadable, or inconsistent; remote access fails closed while local work survives.
- Two attempted owners target the same profile; the existing engine and data remain intact.
- Phone connects to a different Mac or environment with the same label, hostname, IP, or advertised metadata; identity verification rejects it.
- Profile switching cannot retain another profile's authority, trust pin, cache, listener, or wake reason.
- A browser or arbitrary network client tries the existing local-owner IPC; its current Origin boundary and loopback binding remain effective.

### Pairing and session authority

- Missing camera permission, unsupported scanner, denied LAN access, isolated guest Wi-Fi, bad QR, oversized payload, unsupported version, unsafe scheme, wrong certificate pin, or wrong target.
- Expired/cancelled/used QR, two concurrent redemptions, brute-force attempts, host disabled during redemption, and persistence failure before/after consumption.
- Pair reply is lost, phone Keychain write fails, or app crashes during enrollment; no false Connected state or uncontrolled replacement grants.
- A device label attempts to influence authority, two phones use the same label, or a session from another host/profile is replayed.
- Reconnect without authentication, expired/revoked session, unsupported required capability, and stale socket ticket.
- Long-lived bearer or QR credential appears in a URL/log, or the phone stores it in ordinary preferences; reject the implementation at review.
- Disabling the host denies admission before socket close; re-enable requires fresh authentication with a still-valid grant.
- Revoke invalidates all sessions and pending pairings for that device and active streams stop within the documented deadline.
- A restored task backup, old QR, stale cached device list, or pre-rotation certificate cannot resurrect authority.

### Commands and interactions

- A `tasks-read` phone sends a task mutation; denied without a command admission record.
- An ordinary grant attempts terminal operation, credentials, backup/update control, local-owner root claim, or pairing administration; denied.
- A task-control request uses a permitted RPC name with a disallowed nested command variant; denied.
- Request target/project/task is wrong or stale; no action is routed to a third host or unrelated task.
- Duplicate same-ID payload returns the original receipt; same ID with changed payload is rejected.
- Response lost after acceptance, phone reconnect, repeated taps, or process restart does not submit the prompt twice.
- A forged or replayed synced command row with a claimed `issuedBy` value cannot create host admission.
- Queued edits race host delivery or a desktop edit; attachment bytes, revision/lease semantics, and blocked-head order remain correct.
- Steering is unsupported, turn changed, task completed, or cancellation raced admission; UI reports the actual accepted/rejected outcome.
- An approval references an obsolete request/revision or altered command; it cannot approve a replacement request.
- Phone disconnects or is revoked after a task was accepted; accepted task lifecycle follows host policy and unrelated work continues.

### Projection, files and uploads

- Initial snapshot races events, replay window expires, events duplicate or arrive out of order, and a previous socket emits late replies.
- Slow phone or huge tool output cannot grow host queues without bound or stall the engine.
- Reconnect preserves transcript ordering and reports a repair before stale state becomes current.
- Cache from another host/profile, revoked session, or a sign-out cannot be presented as fresh authorized data.
- Upload interruption, duplicate chunk, cancellation, bad hash, invalid media/size, orphaned staging, or attachment referenced by the wrong task.
- File requests escape via traversal, symlink, target substitution, or broad environment paths; denied unless an explicit artifact grant covers them.

### Power and routing

- Wake assertion fails, external power is removed, no task is selected, remote host fails, or the user toggles one of several wake reasons.
- App quits, Mac sleeps or wakes, network switches, DHCP changes address, DNS is stale, or router mapping disappears.
- Domain resolves successfully but the listener is inaccessible; status does not claim external reachability.
- Home IPv4 is behind uncontrolled upstream NAT, only IPv6 is usable, or the phone's outside network cannot reach IPv6.
- Direct route fails; no Tailscale, DERP, cloud tunnel, hosted auth, chat room or R2 fallback is activated.

## Repeatable acceptance journeys

| ID | Journey | Required readback |
| --- | --- | --- |
| E01 | Fresh local-only Mac profile, open Connections, inspect defaults. | No listener/advertisement, empty authority list, local tasks unaffected. |
| E02 | Enable, Add, scan from a physical iPhone on LAN, authenticate. | One consumed pairing, one device grant, correct target, phone and Mac show Connected. |
| E03 | Read a real hosted project's thread and stream a mock-provider run. | Same task ID, transcript updates and final command outcome on desktop and phone. |
| E04 | Create task, send follow-up, queue/edit/remove, steer/stop where supported. | Durable host receipts, exact provider inputs, no duplicate work, correct rejected states. |
| E05 | Trigger a question and a permission request, answer from the phone. | Exact request content/revision, host policy enforced, stale response rejected. |
| E06 | Kill only the test-owned socket/proxy after acceptance and before response. | Same client request ID reconciles to one receipt and one provider execution. |
| E07 | Background/foreground phone and change Wi-Fi/mobile network. | Honest offline state, authenticated reconnect, cursor resume or explicit resnapshot. |
| E08 | Two phones and desktop interact with the same task. | No principal leak, conflict/lease behavior correct, shared outcomes converge. |
| E09 | Disable connections; re-enable; revoke one phone. | Admission denial precedes teardown, history retained, valid peer reconnects, revoked peer fails. |
| E10 | Upload image/file, interrupt transfer, retry and review files/diff. | Correct task-bound byte hash, one attachment, no unauthorized path escape. |
| E11 | Keep awake while plugged in with no selected task; unplug and toggle other wake reasons. | Real assertion readback, correct reason ownership/release, visible failure state. |
| E12 | Mac app restart, explicit sleep/wake, profile switch and damaged authority fixture. | Correct lifecycle, stable identity or explicit recovery, no revived revoked sessions. |
| E13 | Reach the configured direct domain from physical phone cellular with Wi-Fi disabled. | Actual destination route, certificate/host identity, authenticated task operation and reconnect. |
| E14 | No inbound route fixture or real constrained connection. | Accurate direct-only limitation and no undeclared intermediary connection. |
| E15 | Long streaming transcript, scrolling, keyboard, media and accessibility. | Rendering evidence and measured performance against a recorded baseline. |

Use the mock harness for deterministic failure and admission evidence; then run a bounded real-provider journey for the primary supported provider before claiming real task control. Respect provider-specific steering/approval capabilities and record unsupported cases. Do not run an expensive provider merely to test listener wiring.

## Physical device and network acceptance

A simulator cannot complete the camera, permission, backgrounding, network transition, or actual LAN TLS acceptance alone. Test a physical iPhone running the owned LUNA app, the actual signed Mac host, a real QR, and an isolated project fixture. Capture scanner availability and denied-permission recovery.

For Internet acceptance, confirm the actual WAN topology and exact configured public endpoint. A LAN request, loopback curl, DNS answer, valid certificate, or router UI success is insufficient. Disable the phone's Wi-Fi and Tailscale/VPN routes for the direct-only cellular test, then verify the actual authenticated destination and engine task identity. If this environment has no usable inbound route, report E13 as blocked by connectivity and retain a tested LAN release limitation; do not substitute a relay and mark E13 passed.

Verify the absence of a remote-control relay from transport configuration, scoped host connections, client destination, and sanitized network observations. Separate expected provider/API traffic from the remote transport. An absence claim based only on the visible settings page is insufficient.

Native LAN certificate pinning needs tests for correct host, wrong key, changed address, certificate renewal, expiry, and deliberate key rotation. A callback that accepts all server trust fails acceptance even if the QR journey works.

## Performance and power measurements

Record pairing completion time, initial thread load, snapshot size, stream update latency, reconnect/resync time, memory, CPU, and slow-consumer backlog under specified network conditions. Capture native scroll/frame metrics during a long streaming transcript. Establish a baseline and concrete budgets before the rewrite; do not present Zeron's documented design rationale as a LUNA performance result.

For power, inspect the real macOS assertion and observe external-power transitions. Prove it can hold while the host is enabled with no selected task. Prove release on disable/quit and coexistence with an active-task reason. Fake power adapters are useful failure injections but cannot pass real system acceptance.

## Builds and app isolation

Use `scripts/cargo.sh` with `--locked` for affected Rust crates, the project's formatting/lint targets, and the existing Xcode project/toolchain for iOS. Before builds, run `python3 scripts/build-budget.py --status`. Do not bypass a busy/budget guard or silently switch cache policy after a failure.

Follow [the QA infrastructure guide](../../reference/qa-infrastructure.md). Only one LUNA GUI app may run at a time. Let the current app save and quit gracefully before native QA. Set all four QA overrides (`LUNA_DEV_APP_NAME`, `LUNA_DEV_BUNDLE_ID`, `LUNA_DEV_IPC_PORT`, `LUNA_DEV_DATA_DIR`), choose reviewed unused ports and private data, and verify exact process, listening port, and strict code signature. Never refresh the primary identity from a QA checkout or force-quit the user's app to obtain the slot.

After meaningful code changes and passing relevant checks, refresh LUNA Dev for the user. Record build time and any handoff limitation. Preserve/hash QA evidence, gracefully stop the exact reviewed disposable QA app, remove only its scoped bundle when no task needs it, preserve data/cache, and reopen the primary app as required by the project instructions.

## Release gate and completion report

The first release needs passed M0–M4 core journeys, the scoped authorization negatives, physical-phone LAN proof, native performance evidence, and honest supported-route documentation. E13 is required to claim away-from-home direct support on the tested connection; it is not a universal guarantee for every ISP. Any missing V1 row remains a blocker or an explicitly documented reduced release scope accepted by the owner.

Report source commits, native app identities, supported routes, passed/failed/blocked cases, evidence location/hash, exact limitations, and the verified LUNA Dev handoff. Never count a plan, copied screen, socket handshake, static transcript, or unrun scenario as a delivered feature. Browser, Android, Tailscale and relay milestones have their own acceptance and cannot be implied by the first native release.
