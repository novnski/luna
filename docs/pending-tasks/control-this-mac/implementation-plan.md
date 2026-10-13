# Implement direct control of this Mac

This document specifies proposed contracts and work. All new paths, endpoints, fields, and capability names below are design targets, not existing APIs. Read [the scope and stack](README.md) and write the [failure scenarios](validation-plan.md) before implementing a milestone.

## Host architecture

Run the direct host service inside the existing headed engine's Tokio runtime. One profile has one engine owner and one authoritative task store. Window controls, phone clients, and the future browser dispatch to that owner. Do not launch `luna-headless` against the desktop app's open data directory or run a second GUI app to enable remote access.

```text
LUNA iPhone app
  native QR / Keychain / URLSession transport
  owned Rust client projections and transcript rendering
         |
         | direct HTTPS and authenticated WebSocket
         v
LUNA Mac process
  luna-remote listener and remote-access controller
  per-connection RemoteRpcService / paired principal
  engine command admission and durable receipts
  local docs, workspace files, providers, and existing permissions
```

A future browser enters through the same host boundary. It must never gain local-owner privileges from a loopback reverse proxy. Generic local IPC, backup administration, secrets, and app lifecycle control are not published by the remote gateway.

## Proposed source ownership

| Area | Existing or proposed files | Responsibility |
| --- | --- | --- |
| Wire contracts | Existing `crates/proto/src/` and `crates/rpc/src/remote_auth.rs`; proposed `crates/proto/src/remote_control.rs` | Versioned pairing, endpoints, connection/device projections, capability negotiation, scoped requests. |
| Host gateway | Proposed `crates/remote/Cargo.toml` and `crates/remote/src/{host,transport,tls,pairing,connections}.rs` | Direct listener, bounded HTTP and WS processing, TLS trust material, global enable gate, live connection registry. |
| Engine integration | Existing `crates/engine/src/lib.rs`, `rpc.rs`, `doc_host.rs`, and profile store | Start/stop host in the existing runtime; admit authorized remote commands and persist receipts. |
| Desktop UI | Existing `crates/ui/src/settings.rs`; proposed `crates/ui/src/settings/connections.rs` | Control this Mac only, Add QR sheet, paired devices, refresh, disable, revoke, and setup readback. |
| Power | Existing `crates/ui/src/power.rs` and its app integration | Reference-count reasons for task work and remote hosting; observe external power and release assertions correctly. |
| Shared thin client | Proposed `crates/client/` | Host catalog, typed state projections, stream cursors, request IDs, reconnect, and pending-command reconciliation. |
| Mobile facade | Proposed `crates/mobile/` | UniFFI models and callback boundary, rendering snapshots, coalescing and cancellation. |
| Native rendering | Proposed owned ports of required text/markdown/layout modules | Adopt the newer Zeron architecture with measured performance; preserve engine types and license notices. |
| iOS app | Existing `apps/ios/Luna/`, its project and Info.plist; proposed `Connection/` and native UIKit components | QR scan, platform networking, Keychain, saved host entry, direct-mode root, and native feature screens. |
| CLI | Existing `apps/luna/src/remote_cli.rs` | Expose local host status, pairing/session management, enable/disable, and diagnostic readback through documented commands. |

During implementation, refine these modules against the current code layout and keep each milestone bounded. Do not automatically overwrite externally owned guidance, import upstream service URLs, or rebrand provider-owned credentials. Preserve the current iOS app identity and user data. New crate names are LUNA-owned.

## Direct host transport

Use Axum to handle HTTPS application requests and WebSocket upgrades, rustls for TLS, and existing typed RPC frames for engine dispatch. Add limits before deserializing application messages. Keep the current Origin-rejecting local IPC listener separate. V1 production hosting starts from a local-only profile; it must not silently sign into or adopt a synced profile. Preserve existing optional sync features separately and require the ordinary explicit profile transition if the active engine is not in the supported local mode.

For native LAN connections, create profile-owned TLS trust material. The QR carries the expected public-key fingerprint and complete target identity. Native TLS validation must verify possession of that key and the connection's target, rather than globally trusting an arbitrary self-signed server. Scope any custom trust evaluation to the paired host and use a vetted TLS implementation. Certificate renewal, address changes, expiry, SAN/name checks, and key rotation need a passing physical-iPhone acceptance case before shipping; do not solve them with an accept-all callback.

Persist TLS private material in the Mac's credential storage or a reviewed private file with restrictive permissions. Public certificate data is not secret. The first port must be distinct from local-owner IPC, profile-specific, unprivileged, and verified free. Bind only to selected interfaces for the chosen mode. Advertise actual bound endpoints, not wildcard addresses, loopback as a remote URL, or guessed Internet reachability.

LAN access and public direct access use the same authenticated application routes. Public mode requires a deliberate endpoint choice and external routing. A DNS-only A or AAAA record is a hint; the connecting phone must validate the route and target. No auto-enabled router mapping, cloud public-IP collection, or tunnel fallback belongs to V1.

Keep the application API independent of the TLS acceptor so a later local certificate helper can terminate browser-trusted HTTPS without changing command authority. A local reverse proxy must not bypass the enable gate, principal isolation, request scope, host checks, or Origin policy. Trust forwarded headers only from the exact configured local helper.

## Proposed endpoint surface

The concrete REST names below are a design proposal. Their handlers call the same remote authority and engine services; they do not constitute a second command implementation.

| Endpoint | Authentication | Purpose |
| --- | --- | --- |
| `GET /remote/v1/health` | Minimal unauthenticated response | Protocol availability only; expose no task names, user paths, device roster, or credentials. |
| `POST /remote/v1/pair` | One-time QR credential over verified TLS | Redeem the exact target-bound pairing and issue a device session. |
| `POST /remote/v1/session/check` | Device bearer session | Confirm identity, expiry, scopes, and negotiated capabilities. |
| `POST /remote/v1/socket-ticket` | Device bearer session | Issue a single-use, short-lived ticket for one socket upgrade. |
| `GET /remote/v1/ws` | Socket ticket; browser cookie later | RPC and incremental subscriptions, with a fresh principal per connection. |
| `GET /remote/v1/snapshot/...` | Scoped device session | Bounded initial and repair projections from this engine/profile. |
| `POST /remote/v1/commands` | Scoped device session | Validated, idempotent host admission of a supported task command. |
| `POST /remote/v1/uploads/...` | Task-bound upload grant | Bounded staging and attachment transfer before command acceptance. |

No long-lived bearer token goes in a query string. Redact even short-lived tickets from request logs. Pairing credentials are request bodies or a native QR payload consumed locally, not URLs fetched by a hosted landing page. Future browser pairing links can use a fragment, exchange it with this host, and remove it from history.

Where iOS can supply a bearer header for a socket, the same principal model can accept it; the shared ticket route remains available for platforms that cannot. Require full authentication before handling RPC frames or sending project data. Recheck revocation, expiry, enable state, and required scopes for each call and active stream. HTTP pairing and socket authentication must share the same durable grant store.

Negotiate the proposed `remote-direct-v1` capability separately from existing remote authentication. Advertise `remote-task-control-v1` and `remote-interactions-v1` only when the corresponding host admission paths are present. The phone cannot infer support from a matching app version or a visible composer. Preserve existing authenticated SSH/read-client compatibility where deliberate; migrate stored authority records under the profile owner with missing new grants defaulting to denied.

## Pairing flow

1. The owner enables Allow connections on the Mac. The host validates profile authority storage, TLS material, the bind address, and the port. The UI shows the actual ready state or the precise failure.
2. The owner selects Add. The host creates a pairing with a cryptographically random credential, a five-minute expiry, one redemption, and an explicit initial scope set. The selected route and target come from the host, not phone metadata.
3. The Add sheet renders a native `luna://pair` QR payload. Proposed fields are format/protocol version, pairing ID, one-time credential, expiry, device ID, environment/profile ID, workspace scope, capabilities, endpoint candidates, and host TLS public-key fingerprint. No provider credential or reusable session token belongs in it.
4. The phone opens its pairing screen, checks QR format and bounds, asks for camera/local-network permissions where necessary, and shows the Mac name and destination. Validate every endpoint against the QR target and trust pin before sending the secret. Restrict schemes to the supported HTTPS direct transport.
5. The phone redeems the credential over the verified connection. The host atomically checks enable state, expiry, target, capabilities, and unused status; consumes the pairing and persists a distinct session before returning it. The client's device label is display metadata, not authority.
6. The phone saves the reusable session in Keychain, keyed by target and local app profile. It saves public endpoint and trust metadata separately. If response delivery is lost or Keychain persistence fails, show an uncertain pairing outcome. V1 recovery is to revoke that specific attempted grant locally on the Mac and create a fresh QR; do not automatically issue replacement grants or display Connected without a validated session. A transparent recovery protocol is a later enhancement with its own proof and replay policy.
7. The phone authenticates and receives host capabilities and an initial workspace projection. The Mac records the authenticated connection and displays the paired phone. Both show the same stable target identity.
8. Cancel, expiry, Disable connections, or closing the Add sheet invalidates the unused pairing. A scanned screenshot of an expired or used QR cannot issue another session.

Opening Add is the owner's authorization to pair one device with the displayed scope. The proposed ordinary release grant covers environment/task reads, scoped task operation, interaction responses, and registered-workspace/explicit-artifact reads. Terminal operation is excluded. Do not add repeated confirmations for the ordinary scan-and-connect flow. Sensitive optional grants require an explicit host choice. Rate-limit invalid pairing attempts, bound concurrent pending pairings, and make parallel-redemption and uncertain-persistence behavior explicit.

A fallback import/paste flow must contain equivalent trust and entropy to the QR. Do not substitute a permanently valid short PIN. Separate revoked pairing credentials from existing device-session revocation.

## Authority and persistent device records

Extend the existing remote authority record rather than derive authorization from the registry's device list. Proposed additions include a stable paired-device ID, label, platform metadata, associated sessions, creation time, last authenticated connection, allowed scope set, revocation metadata, and remote-enable generation. Metadata can be edited locally without changing the target identity.

The current host stores credential hashes and checks target identity in `RemoteRpcService`. Retain that approach. A damaged, unreadable, or permissively accessible authority store disables remote hosting and pairing while leaving local work available. A task backup or restored transcript must not silently restore a revoked phone's grant.

Disabling remote access must publish a denial gate before socket teardown. Any later admission observes that gate. Re-enable requires valid sessions to authenticate again. Revoke access for a phone atomically revokes its sessions and pending pairings before closing its connections. Neither button grants another device's scopes, stops unrelated work, nor erases task history.

Initial device sessions should use the existing bounded expiry model; the maximum currently supported is thirty days. Choose the product default explicitly in implementation and show expiry/re-pairing state. A renewal design may be added only with scoped authority, rotation, and replay/deduplication acceptance; do not invent an unlimited refresh credential in V1. Reinstalling the phone, wiping the profile, or rotating trust can require local re-pairing.

## Method scopes and engine admission

Add a narrowly defined `tasks-operate` scope for project-bound create/send/queue/steer/stop and ordinary task lifecycle actions. Separate `interactions-respond` for questions and approvals if the existing task command API cannot cleanly distinguish them. Keep `files-read` and `terminal-operate` separate, with terminal access off in the default QR grant. Existing `environment-read` and `tasks-read` remain the basis for metadata and streams.

The current method policy denies task mutations remotely. Do not enable `ExecuteTaskCommand` wholesale merely by mapping it to one scope. Inspect the command variant and exact target, validate all associated projects/tasks/paths, and authorize only the permitted operation. Account/credential administration, updater control, backup management, local-owner root claim, arbitrary settings mutation, and remote session administration remain local-only.

Remote admission must produce the same durable engine command receipt as local actions and the exact admission record used by the task drain. The session's principal, scope, source device, command ID, target, and payload must be bound before execution. A phone cannot obtain authority by editing `issuedBy` or pushing a command row into a doc.

Preserve existing sandbox and provider permission behavior: Full and Auto never prompt ([permission modes](../permission-modes/README.md)); only legacy Ask chats and LUNA ceiling grants ask. A phone's grant to operate a task does not approve every command that task requests. Approval responses must match the outstanding request ID, request revision/content, task identity, and valid session; a stale approval cannot authorize a new command. Define persistent approval choices through the same owned policy used locally.

For files and uploads, authorize the task's actual workspace plus explicitly granted artifacts. Check traversal, symlink resolution, canonical target, size, and staged upload ownership. Do not copy T3's broader environment-file semantics without choosing that policy for LUNA. No file endpoint returns host credential-store contents by default.

Every submitted mutation carries a stable client request ID. Repeating the same target, operation, and payload returns the original result/receipt without executing twice. Reusing the ID with a changed payload is rejected. An HTTP timeout is an uncertain outcome, not permission to generate a new ID and resend. Query the host's receipt before deciding to retry.

## Thread projections and streaming

The Mac is the authoritative source for projects, active and archived threads, task status, transcripts, tool results, questions, queues, file changes, and command outcomes. Initial phone load obtains a bounded snapshot and a cursor under one consistent view, then subscribes to changes after that cursor. Audit current watch/snapshot methods individually: existing local methods such as the broad workspace watches are not automatically covered by today's remote method inventory. Add narrowly scoped remote projections where exposing the local method would reveal unrelated data.

Use a host event sequence or explicit projection revisions with a replay window. Events identify the profile, project/task, and stream generation. Apply them incrementally in the client; detect gaps and ask for a repair snapshot. If the replay window has expired, resnapshot. Do not assume every existing RPC stream already has a resumable cursor or replay log.

Keep existing Loro documents as local storage/engine implementation where useful. The direct phone receives engine-owned projections or read-only document updates. It does not become a cloud-room writer or an independent authority over commands. Optimistic sends remain provisional until the host returns an admission receipt; projected outcomes settle or reject them.

Closing or backgrounding the phone does not cancel accepted work. On foreground, verify path, TLS trust, target, session, and capabilities; reconnect with bounded backoff/jitter and resume or repair subscriptions. A generation check discards late replies from the previous socket. Distinguish session expired, session revoked, unsupported version, host offline, lost route, and stale projection.

Implement slow-consumer limits and cancellation before enabling large tool output or media. Fetch large bodies lazily with bounded pages/ranges and task grants; do not flood the transcript socket. Local phone cache belongs to that exact host/profile and must not be displayed under a different paired Mac. Revocation blocks future retrieval; it cannot erase information already viewed on another device.

## Native app migration

The existing LUNA iOS app already has an owned app root, transcript, composer, queue, and registry/room clients. The new Zeron implementation separates UIKit painting from Rust client state and layout. Port the useful native shell and rendering modules into LUNA-owned crates and reconcile their schemas against current `luna-proto` and `luna-doc` before importing behavior.

First prove a small transport/UniFFI vertical slice on a physical iPhone: scan, authenticate, display one real task projection, receive a stream, cancel it, and reconnect. Then expand rendering and composer features. Keep the existing demo useful during migration, but demo data cannot pass live remote acceptance.

Give the shared Rust client a transport boundary. For iOS, Swift owns `URLSession`, trust challenges, Keychain, local-network/camera permissions, and background/foreground lifecycle. It passes validated frames to the Rust state/projection core and sends the core's outbound requests. Do not keep two competing networking/reconnect state machines or expose actor-unsafe callbacks. Verify cancellation and lifetime behavior before making this the portable foundation.

Declare `NSCameraUsageDescription`, `NSLocalNetworkUsageDescription`, and the selected `_luna-remote._tcp` service under `NSBonjourServices` where discovery is used. Bonjour publishes only bounded discovery metadata; the QR establishes trust. Prefer the in-app scanner so the pairing secret stays inside LUNA; a custom scheme handler accepts only the validated pairing format and must not send the payload to a browser or analytics service. Scope native session Keychain entries by host and app profile, use a device-only accessibility policy suitable for foreground operation, and disable credential synchronization. Local permission denial and credential-store failure have their own user-visible recovery states.

Replace direct-mode dependencies on WorkOS organization selection, edge registry rooms, chat rooms, device-room relay, R2 attachments, and nudges. The phone's home screen starts from saved paired Macs; V1 actively controls only the selected Mac. A disabled or offline Mac preserves its saved entry and cached history without pretending to run work locally.

Import T3 features by mapping their engine contracts and rebuilding native screens. Do not copy React Native components into UIKit, emulate every T3 endpoint, or run the T3 server alongside LUNA. Preserve license notices for reused code and owned bundle/update identities.

## Mac power and lifecycle

Reuse the IOKit power adapter but separate reasons: selected-task work and remote-host availability. One shared coordinator holds an assertion while any eligible reason is active and releases it when none is active. A toggle for one reason must not cancel another task's valid reason.

For the screenshot's remote setting, the predicate is user opt-in AND remote host enabled/ready AND external power connected. It is not tied to the selected task or a live phone socket. Observe power-source changes and listener failures. Failures are visible and retryable; do not claim the Mac is protected from idle sleep when acquisition failed.

Keep display sleep permitted. Explicit system sleep, unsupported lid-closed behavior, logout, shutdown, and process exit can still stop availability. Use the system API rather than a broadly managed `caffeinate` process or global sleep-policy change. The assertion and listeners are released by scoped ownership on disable, shutdown, or fatal startup failure.

## Setup diagnostics and future transports

Show three independent results: host service ready, phone authenticated, and route proven from this phone. Record last authenticated connection time, not arbitrary file modification time or metadata refresh time. The Add sheet indicates whether its address is nearby-only or explicitly configured for outside access.

Direct-domain setup uses the existing DNS provider. Store credentials narrowly and locally; show exact record changes before applying them. V1 can ship with documented manual DNS/router setup and an in-app endpoint field. Port automation, DNS automation, and external reachability probes must each have a separate exact-scope implementation and validation step.

The transport interface should allow a later Tailscale route or cloud relay without changing task authority. Both remain absent/disabled in V1. Normal Tailscale may use encrypted relays, including initial negotiation, and requires external coordination. It cannot be described as satisfying this no-relay release. A future relay transports authenticated frames; it never grants engine permission on its own.

Browser access is a later client milestone against the same API. It needs trusted HTTPS, a responsive UI, browser-origin and CSRF controls, host-issued cookies, socket-upgrade authorization, and recovery independent of one lost phone. Native TLS pins cannot be copied into a normal browser page as a substitute for browser certificate trust.

APNs notification delivery, native Android, generic remote desktop control, Computer Use, browser automation, provider account administration, and every remaining T3 workflow need explicit inventory and policy decisions. None is automatically enabled by adding the transport. Foreground status and alerts can ship without third-party push.

## Delivery order

For each M0–M4 milestone, define failures and acceptance artifacts first, implement the relevant host and client path, run focused builds and E2E checks, and inspect the evidence before broadening scope. A checkpoint must state whether it supports pairing, reads, or actual task operation; do not call all three equivalent.

Use the repository's locked Cargo wrapper, build budget, native app isolation, and owned iOS signing. After meaningful Mac code changes, refresh and verify LUNA Dev as required by `AGENTS.md`. Documentation-only planning does not require an app rebuild. Preserve unrelated work and follow the repository's commit-and-push workflow only after the milestone's actual required checks pass.
