# Control this Mac from a paired phone

**Status:** Implementation plan prepared on 1 October 2026. No remote host, mobile migration, network configuration, or feature acceptance described here has been implemented by this planning task.

LUNA should let its owner open Settings → Connections → Control this Mac, enable connections, select Add, and scan a QR code with the LUNA iPhone app. The phone then follows and controls the same projects and threads that run on this Mac. The Mac owns execution, provider credentials, task storage, permissions, and accepted commands. The phone is a client, not another execution host.

The first version must use direct connections and require no relay, hosted identity service, VPS, cloud workspace, or externally hosted transcript storage. The Mac must stay awake, run LUNA, and remain reachable. QR pairing establishes trust and permission; it does not create an Internet route. Nearby access can work over the LAN. Away-from-home access requires a configured direct route to this Mac.

This plan supersedes the earlier suggestion to make Tailscale the preferred first transport. The user's latest explicit requirement is no relay in V1. Ordinary Tailscale includes relayed negotiation and fallback, so it is a later optional transport, not a hidden V1 dependency.

## Read the plan

- [Implementation steps and contracts](implementation-plan.md): stack, source changes, pairing, authorization, streaming, power, and milestones.
- [Mobile and browser feature parity](feature-parity.md): scope of the first release and the complete inventory needed for later parity.
- [Failure cases and E2E validation](validation-plan.md): repeatable acceptance, physical-phone evidence, network conditions, and release gates.

## Product boundary

The supplied screenshot shows Connections with Control this Mac selected, Add and refresh actions, an Allow connections toggle, paired-device rows with Last connected and Revoke access, and a Keep this Mac awake setting. Those controls are the interaction reference.

Implement only Control this Mac in the new settings surface. Do not add Control other devices or SSH tabs, an outbound host-enrollment flow, remote compute provisioning, or task handoff between machines. Existing SSH functionality elsewhere in LUNA remains outside this feature; this plan does not authorize removing it.

The iPhone can start and continue project work on the paired Mac, follow output, send or queue messages, steer where the provider supports it, stop work, answer questions and approval requests, and review files and changes within the granted scope. The host's existing provider permissions and sandbox behavior remain authoritative.

The user's original browser goal is retained: a browser companion should use the same host API in a later phase. Losing the installed phone must not permanently remove access; add another pairing or a separately designed recovery route. A browser is not automatically trusted because the native app has paired.

## Codex is an interaction reference

The screenshot does not establish Codex's network transport. OpenAI's current documentation says Codex Remote uses a secure relay to keep authorized devices connected. LUNA will reproduce the selected interaction while using a different V1 transport. The source also confirms QR pairing and the need for an awake, online host. See [Remote connections](https://learn.chatgpt.com/docs/remote-connections).

Consequently, V1 cannot promise Codex's router-independent setup across every Internet connection. If neither a permitted inbound IPv4 route nor usable inbound IPv6 exists, the app must explain that direct remote access is unavailable. It must not silently activate a relay or describe a LAN-only listener as globally reachable.

## Settings behavior

| Control | Required behavior |
| --- | --- |
| Allow connections | Off by default. Enabling starts the scoped direct host listener. Disabling stops new remote admission, cancels pending pairings, closes remote streams, and releases the remote-host power reason. |
| Add | Creates one short-lived, single-use pairing for this exact Mac and active profile. Shows a QR code, expiry, selected endpoint, and Cancel. |
| Paired-device list | Comes from the host's remote authority store, not the synced device registry. Shows label, granted access, connected state, and last successful authenticated connection. |
| Refresh | Refreshes the local authority and connection readback. Does not change a grant or create a pairing. |
| Revoke access | Revokes all remote sessions and outstanding pairing credentials belonging to that phone for this profile, disconnects it, and denies further commands. |
| Keep this Mac awake | Opt-in. Holds an idle-sleep assertion while remote hosting is enabled and external power is connected, including when no task is selected. |

Disabling connections pauses access without deleting paired-device history. Turning it back on may let a still-valid paired phone reconnect. Revoke access is the permanent authorization action. Neither action automatically stops tasks already accepted by the Mac; show that behavior in the setting's supporting text.

Closing the last window may leave LUNA running according to its existing lifecycle. Explicit Quit stops this V1 host. Do not install a background daemon or change quit semantics in this feature. Keep awake does not promise availability after explicit sleep, shutdown, app quit, network loss, or unsupported closed-lid operation.

The paired phone must report Disconnected, Connecting, Connected, or Access revoked from actual transport and authentication state. Cached transcripts remain readable with a stale/offline indication. Do not show Connected merely because a TCP socket opened or a registry heartbeat exists.

## Chosen stack

Everything below is a proposed implementation choice, not an installed dependency or existing service.

| Layer | Choice | Reason |
| --- | --- | --- |
| Mac execution | Existing `luna-engine`, provider harnesses, local task store, and command admission | Reuse the authoritative runtime; do not create a second engine or database owner. |
| Remote authority | Extend `luna-rpc::RemoteAuthStore` and a fresh `RemoteRpcService` per remote connection | Preserve target identity, method scopes, expiry, revocation, and fail-closed policy. |
| Host gateway | New owned `luna-remote` Rust crate using Tokio, Axum HTTP/WebSocket handling, and rustls TLS | Keep the host in the existing Rust runtime and make direct access independent of a cloud edge. |
| iPhone application | Existing LUNA app identity, with the newer Zeron UIKit shell and Rust rendering/client separation ported into owned LUNA code | Follow LUNA's native lineage and avoid importing T3's complete React Native product. |
| Shared mobile state | New owned `luna-client` and `luna-mobile` crates, with UniFFI bindings | Share typed projections and rendering behavior without running an agent engine on the phone. |
| iOS transport | Native `URLSession` HTTPS/WebSocket adapter feeding the shared client core | Keep Apple trust evaluation, network lifecycle, camera permissions, and Keychain integration at the platform boundary. |
| Pairing scanner | VisionKit QR scanning with an AVFoundation fallback and paste/import fallback | Handle unsupported cameras and denied permissions; never require opening a third-party website. |
| Secrets | Mac-owned private TLS/authority material and iOS Keychain device sessions | Keep reusable credentials out of preferences, QR history, logs, and sync documents. |
| LAN discovery | Bonjour metadata plus direct endpoints encoded in the QR | Discovery is a convenience, never an authentication source. |
| Public domain | DNS-only record to the home route; explicit router forwarding or inbound IPv6 | Support the existing domain without a rented server or tunnel. |
| Browser later | A responsive web client against this same API, with trusted HTTPS and host-issued browser sessions | Preserve the any-device goal without a second application backend. |

Axum supports WebSocket upgrades and rustls integration is available through `axum-server`. Resolve compatible crate versions during the implementation milestone and commit `Cargo.lock`; the plan does not prescribe an unverified dependency combination. Sources: [Axum WebSocket support](https://docs.rs/axum/latest/axum/extract/ws/), [Axum server TLS](https://docs.rs/axum-server/latest/axum_server/tls_rustls/index.html).

Apple requires an appropriate local-network purpose description for direct LAN use. QR scanning requires camera permission and scanner availability checks. Sources: [Local-network purpose](https://developer.apple.com/documentation/bundleresources/information-property-list/nslocalnetworkusagedescription), [Data scanner](https://developer.apple.com/documentation/visionkit/datascannerviewcontroller).

## Direct route choices

| Mode | V1 behavior | Limitation |
| --- | --- | --- |
| Nearby phone on the same reachable LAN | Mac serves HTTPS on a dedicated unprivileged remote port; phone verifies the QR-established host trust. | Guest Wi-Fi isolation, local-network permission denial, and firewalls can block it. |
| Phone away from home | Owner configures the direct public endpoint and router/firewall route; the same paired identity and API apply. | Ordinary port forwarding cannot bypass uncontrolled upstream NAT or ISP filtering. |
| Domain with changing home address | Optional provider-specific DNS update from the Mac using narrowly scoped credentials. | DNS caching can delay reconnection; provider setup is not universal. |
| No direct inbound route | Report a precise setup limitation. Retain local and nearby operation. | No relay-free software promise can make this route universally available. |

Choose an available dedicated host port at setup and persist it per profile. Do not reuse the local-owner IPC port, assume a QA port is free, or require root binding. A router can map public TCP 443 to the Mac's unprivileged HTTPS port. Make router configuration explicit; automatic UPnP/PCP mapping is deferred until a separately reviewed implementation can preserve existing mappings and show exact scope.

For LAN native access, bootstrap trust with a host certificate/public-key pin carried by the local QR. Never accept every self-signed certificate. For public native access, use the same pinned identity with the configured endpoint; a hostname alone is not the trust anchor. A future browser requires a browser-trusted certificate, because it cannot implement the native pinning flow through ordinary page JavaScript. A local Caddy helper is a candidate for later certificate automation, not a relay. Certificate issuance does not establish reachability. Sources: [DNS-only traffic](https://developers.cloudflare.com/dns/proxy-status/), [certificate challenges](https://letsencrypt.org/docs/challenge-types/).

No Tailscale, DERP, Cloudflare tunnel, WorkOS login, cloud room membership, APNs service, or R2 upload participates in V1 direct remote control. Existing optional sync profiles and provider network calls are separate features; the new host must work with a local-only LUNA profile. Direct remote control still transmits selected information to the paired phone and may retain a phone cache.

## Existing foundations and blockers

| Current source | Confirmed foundation or required change |
| --- | --- |
| [Remote authority](../../../crates/rpc/src/remote_auth.rs) | Pairing and device sessions exist, but only environment/task/file reads and terminal operations are remotely scoped. New task-control authority is required. |
| [Engine service factory](../../../crates/engine/src/lib.rs) | `remote_rpc_factory` creates a separate authorization guard per socket. Reuse this boundary from the headed app. |
| [Local RPC transport](../../../crates/rpc/src/server.rs) | Rejects browser Origin headers and serves trusted local IPC. Keep it private and unchanged in purpose. |
| [Task contract](../../../crates/proto/src/task_command.rs) and [engine RPC](../../../crates/engine/src/rpc.rs) | Durable command receipts and authoritative admission exist. Integrate remote principals there; do not admit arbitrary synced rows. |
| [Command drain](../../../crates/engine/src/doc_host.rs) | Exact local admission records are required for executable command rows. The inherited mobile ledger-writing path cannot substitute for host admission. |
| [Current device settings](../../../crates/ui/src/settings/devices.rs) | This is a registry page. It does not represent paired remote authority or connection permission. Add a distinct Connections page. |
| [Power adapter](../../../crates/ui/src/power.rs) | Existing assertion is tied to a selected working task. Add a separate remote-host reason and coordinate all reasons. |
| [iOS root](../../../apps/ios/Luna/App/AppModel.swift), [relay client](../../../apps/ios/Luna/Sync/DeviceRelayClient.swift), and [session store](../../../apps/ios/Luna/Sync/SessionStore.swift) | Current phone path is edge/account/room-based. Add local pairing and replace direct-mode networking and command admission. |
| [iOS project](../../../apps/ios/Luna.xcodeproj/project.pbxproj) | Preserve the owned `com.novnski.luna.ios` identity, signing, app data, and existing migration rules. |

## Milestones

1. **M0: Prove the boundary.** Complete the feature inventory and failure cases, confirm a physical phone can reach a harmless pinned-TLS host endpoint, and verify a native transport callback can feed the Rust core.
2. **M1: Pair and revoke.** Deliver the Connections screen, direct listener, QR flow, durable device/session records, connection readback, and disabling/revocation.
3. **M2: Follow real work.** Deliver host-owned project/thread snapshots, incremental transcript streams, reconnect/resume, and honest offline state on the phone.
4. **M3: Continue real work.** Deliver scoped creation, send/queue/steer/stop, questions and approvals, attachments, and engine-owned idempotent admission.
5. **M4: Native first release.** Complete the native rendering migration, file/diff review, power/lifecycle handling, direct-domain setup, and physical-device E2E gates. Advanced features remain explicitly tracked in the parity inventory.
6. **M5: Extend the clients.** Add browser access, remaining T3 workflows, optional Android, and independently selected networking transports. Do not add a relay until the owner changes that requirement.

The first release includes M0–M4. Pairing-only or read-only milestones are development checkpoints, not a completed Control this Mac feature.

## Evidence baseline

| Repository | Source revision used in this conversation |
| --- | --- |
| LUNA | `e583d52a5e96b0223a6c012b50a6ab0bc99c6b7d` |
| BB | `2573ad5e85835dc1c90aa33795e6dba52ef5aa92`, after an SSH fast-forward pull |
| T3 | `5cc99e1c23980d7995a13c47f969b47cb68ed1be`, fetched source; local checkout preserved |
| Zeron | `b42fc2b8fbf247dd92796c2917f277535cea91ac`, matching the configured personal fork |

Useful source references are [Zeron's native rewrite](https://github.com/novnski/zeron/blob/b42fc2b8fbf247dd92796c2917f277535cea91ac/docs/mobile-rewrite.md), [its mobile crate](https://github.com/novnski/zeron/blob/b42fc2b8fbf247dd92796c2917f277535cea91ac/crates/mobile/Cargo.toml), [T3 direct authentication](https://github.com/pingdotgg/t3code/blob/5cc99e1c23980d7995a13c47f969b47cb68ed1be/docs/internals/environment-auth.md), [T3 direct pairing](https://github.com/pingdotgg/t3code/blob/5cc99e1c23980d7995a13c47f969b47cb68ed1be/docs/user/remote-access.md), and [BB's browser model](https://github.com/get-bb/bb/blob/2573ad5e85835dc1c90aa33795e6dba52ef5aa92/docs/multiple-devices.md).

Recheck these contracts before implementation. Source review and supplied screenshots establish the design baseline; they are not fresh mobile, reachability, performance, or remote execution acceptance evidence.
